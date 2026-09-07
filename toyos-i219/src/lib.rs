//! The Intel I219's driver — every decision it makes, and none of the
//! instructions that carry them out.
//!
//! The part is the T14's onboard `8086:15fc` at `00:1f.6`. Its register file is
//! the one the *Intel 82574 GbE Controller Family Datasheet* (317694-018, rev
//! 2.7) defines, which QEMU's `e1000e` model — `8086:10d3`, the 82574L itself —
//! implements too; that is why one driver drives both, and why the datasheet
//! this file cites throughout is the 82574's rather than the I219's own, which
//! describes the part and not the register set. Every `§` below is a section of
//! it.
//!
//! # The boundary
//!
//! Four traits, named by what they do and not by what chip is behind them:
//! [`Registers`] is one memory-mapped access, [`Clock`] is one counter read,
//! [`DmaBuffers`] is the memory the device and this process share, and
//! [`Interrupts`] is a message arriving. Each is a generic parameter resolved
//! at compile time — no trait object, no dispatch — and each real
//! implementation is one instruction's worth of meaning: a volatile load, a
//! volatile store, a fence, a counter read, a field read. Everything with a
//! branch is above the boundary, in this crate, where the host tests it.
//!
//! In netd those four are the substrate's own: the mapped BAR, the monotonic
//! clock, a `DmaRegion` in this function's IOMMU domain, and the claim's
//! interrupt record. On the host they are `stub.rs`, which is the datasheet
//! written down.
//!
//! # The device is not trusted
//!
//! Every number in a written-back descriptor is the device's, and this driver
//! is on the far side of an IOMMU domain from the rest of the machine but on
//! the *same* side as its own memory. A length longer than the buffer it was
//! given becomes the length of a slice netd hands to smoltcp, so
//! [`parse_rx`] bounds it, and [`RxRefusal`] is every way a written-back
//! descriptor is refused rather than believed.
//!
//! # What the specification permits, and this driver therefore expects
//!
//! - **A completion is `DD` in memory, never `RDH`.** §7.1.8: hardware "writes
//!   back used descriptors just prior to advancing the head pointer(s)", and
//!   the head register is a shadow that "includes those descriptors completed
//!   but not yet stored in memory". A driver reading `RDH` to find completions
//!   reads descriptors whose data has not arrived.
//! - **Write-back is batched and opportunistically ordered** (§7.1.7.1), so the
//!   ring is walked one descriptor at a time from where it was last left and
//!   stops at the first `DD` that is clear — never scanned for the highest one
//!   set.
//! - **A descriptor may come back empty.** §7.1.7.2's null descriptor padding
//!   writes back `DD` "and all other bits unchanged".
//! - **An interrupt may mean nothing.** §7.4.5 names the spurious interrupt by
//!   name, and §10.2.4.1's case 3 says a read of `ICR` with no interrupt
//!   asserted has no side effect — so the causes this driver acts on are
//!   written back to `ICR` explicitly rather than assumed cleared by the read.
//! - **The link goes away.** §10.2.4.1: `LSC` "is set whenever the link status
//!   changes (either from up to down, or from down to up)".

#![no_std]
#![forbid(unsafe_code)]

/// The test harness links `std` whatever this crate is; the stub and the tests
/// use it for a seeded model's collections and nothing else compiles against
/// it.
#[cfg(test)]
extern crate std;

pub mod regs;

#[cfg(test)]
mod stub;
#[cfg(test)]
mod tests;

use regs::{cause, ctrl, rah, rctl, rx_desc, status, tctl, tx_desc, txdctl};

/// One memory-mapped register access.
///
/// **The offsets are this crate's own constants**, every one of them under
/// [`regs::REGISTER_BYTES`], which [`I219::open`] refuses a smaller window
/// than. That is the whole of what makes an implementation free to be one
/// volatile access with no bound of its own.
pub trait Registers {
    /// Bytes the window covers. A field read.
    fn len(&self) -> usize;
    /// One volatile 32-bit load. Volatile because the device writes the same
    /// bytes, and a plain load of a status register can be hoisted out of the
    /// loop that waits on it.
    fn read(&self, reg: usize) -> u32;
    /// One volatile 32-bit store.
    fn write(&self, reg: usize, value: u32);
}

/// One counter read: nanoseconds on a clock that does not go backwards.
pub trait Clock {
    fn nanos(&self) -> u64;
}

/// The memory this process and the device both reach.
///
/// **Two addresses, because neither derives from the other**: the offsets this
/// driver computes are where its own loads and stores land, and
/// [`Self::device_addr`] is what a descriptor must carry for the device to
/// reach the same bytes through the unit.
///
/// The two fences are here rather than beside the doorbell because the
/// ordering they impose is over *this* memory: the architecture's rules about
/// when a store to DMA memory becomes visible to a device live in the
/// implementation and nowhere above it.
pub trait DmaBuffers {
    /// Bytes in the grant. A field read.
    fn len(&self) -> usize;
    /// Where the device reaches byte `at`. Never a physical address once a
    /// unit translates for this function.
    fn device_addr(&self, at: usize) -> u64;
    /// One volatile aligned 64-bit load of the grant.
    fn read(&self, at: usize) -> u64;
    /// One volatile aligned 64-bit store into the grant.
    fn write(&self, at: usize, word: u64);
    /// One release barrier: every store made above this call is visible to the
    /// device before any store made below it. Rung before a tail register is,
    /// so a device that fetches on the doorbell cannot fetch a descriptor that
    /// is half-written.
    fn publish(&self);
    /// One acquire barrier: every load made below this call sees memory at
    /// least as new as the load above it that found `DD` set. Taken before a
    /// descriptor's other fields are read, so a written-back length cannot be
    /// read from before the write-back.
    fn observe(&self);
}

/// A message arriving on this function's interrupt.
pub trait Interrupts {
    /// How many have arrived since the last call; zero for none. The count is
    /// never what a message *meant* — that is in `ICR` and in the rings.
    fn taken(&self) -> u32;
}

/// How many receive descriptors the ring holds.
///
/// §7.1.8: `RDLEN` "must be a multiple of 128", so the count is a multiple of
/// eight. Two hundred and fifty-six 2048-byte buffers is half a megabyte of
/// grant and a millisecond of gigabit line rate — enough that a pass this
/// driver's caller spends elsewhere does not overrun the ring.
pub const RX_RING: usize = 256;
/// How many transmit descriptors the ring holds. Sixteen, because a frame is
/// handed to the device the moment it is filled and reclaimed on the next
/// send: what this bounds is how many may be in flight at once.
pub const TX_RING: usize = 16;
/// Bytes per receive buffer — `RCTL.BSIZE = 00b` with `BSEX` clear
/// (§10.2.5.1).
pub const RX_BUF_BYTES: usize = 2048;
/// Bytes per transmit buffer. One frame each, and `RCTL.LPE` is clear so
/// nothing on this link is longer than 1522 bytes.
pub const TX_BUF_BYTES: usize = 2048;

const _: () = {
    assert!(RX_RING * rx_desc::BYTES % 128 == 0);
    assert!(TX_RING * tx_desc::BYTES % 128 == 0);
    assert!(RX_RING <= u16::MAX as usize && TX_RING <= u16::MAX as usize);
};

/// The grant's layout. One grant, because every byte of it is this process's
/// own: what a wrong offset here corrupts is this driver's memory and nobody
/// else's.
const OFF_RX_RING: usize = 0x0000;
const OFF_TX_RING: usize = OFF_RX_RING + RX_RING * rx_desc::BYTES;
const OFF_RX_BUFS: usize = 0x2000;
const OFF_TX_BUFS: usize = OFF_RX_BUFS + RX_RING * RX_BUF_BYTES;
/// How much memory the driver needs granted, in one region.
pub const GRANT_BYTES: u64 = (OFF_TX_BUFS + TX_RING * TX_BUF_BYTES) as u64;

const _: () = {
    assert!(OFF_TX_RING + TX_RING * tx_desc::BYTES <= OFF_RX_BUFS);
    // §7.1.8 and §7.2.4: the ring base "is aligned on a 16-byte boundary" and
    // "hardware ignores the lower 4 bits", so a ring at an unaligned offset
    // would be programmed somewhere else entirely.
    assert!(OFF_RX_RING % 16 == 0 && OFF_TX_RING % 16 == 0);
};

/// Frames this driver hands up in one pass before it says there are no more.
///
/// **A ring that never empties is a ring the specification permits**: at line
/// rate the device refills descriptors as fast as they are returned, and a
/// caller that loops until the ring is empty never returns to its other work.
/// The budget is what makes "no more this pass" a thing this driver can say —
/// [`I219::begin_pass`] is where it is given back.
pub const RX_BUDGET: u32 = 64;

/// Why the function was not brought up. Each keeps its own word: a caller
/// asks different things of a grant that is too small and of a part with no
/// station address.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
    /// The register window is smaller than the register file this driver
    /// reaches, so an access it makes would land outside the mapping.
    Window { given: usize, needed: usize },
    /// The grant is smaller than the rings and buffers need.
    Grant { given: usize, needed: usize },
    /// Every register reads ones: nothing routes this window, and there is no
    /// device behind it.
    Dead,
    /// `CTRL.RST` did not clear itself (§10.2.2.1) inside the deadline.
    ResetUnfinished { after_nanos: u64 },
    /// `RAH0.AV` is clear, so §10.2.5.23's "if no NVM is present" arm is what
    /// this part answered and it has no station address to filter on.
    NoStationAddress,
    /// A register did not read back what was written to it, so this window is
    /// not the register file it was taken for.
    NotAccepted { reg: usize, wrote: u32, read: u32 },
}

impl core::fmt::Display for Refusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Window { given, needed } => write!(
                f,
                "its register window is {given:#x} bytes and this driver reaches {needed:#x}"
            ),
            Self::Grant { given, needed } => write!(
                f,
                "the DMA grant is {given:#x} bytes and its rings and buffers need {needed:#x}"
            ),
            Self::Dead => {
                write!(f, "every register in the window reads ones, so nothing routes it")
            }
            Self::ResetUnfinished { after_nanos } => write!(
                f,
                "CTRL.RST was still set {after_nanos} ns after it was written, and the \
                 datasheet says that bit is self-clearing"
            ),
            Self::NoStationAddress => write!(
                f,
                "RAH0.AV is clear, so it has no station address loaded from its NVM and \
                 nothing would pass its receive filter"
            ),
            Self::NotAccepted { reg, wrote, read } => write!(
                f,
                "register {reg:#x} was written {wrote:#x} and reads back {read:#x}"
            ),
        }
    }
}

/// Why a written-back receive descriptor was refused. The device wrote it, so
/// each of these is a claim about this driver's own memory and the only safe
/// answer is to drop the frame and give the buffer back.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RxRefusal {
    /// More bytes than the buffer the descriptor was given. The one that
    /// matters most: this number becomes the length of a slice.
    Length { len: u32 },
    /// §7.1.3.4's error bits, valid because `EOP` and `DD` are both set.
    Errors { errors: u8 },
    /// `DD` without `EOP`: with `RCTL.LPE` clear no frame can span two
    /// 2048-byte buffers, so a device that split one is not answering the
    /// configuration it was given.
    Split,
    /// Nothing in it — §7.1.7.2's null descriptor padding, or a frame shorter
    /// than an Ethernet header.
    Empty { len: u32 },
}

/// What the link is doing, as `STATUS` last answered.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Link {
    pub up: bool,
    /// Megabits per second, from `STATUS.SPEED` (§10.2.2.2); 0 while down.
    pub speed_mbps: u16,
    pub full_duplex: bool,
}

/// One received frame: where its bytes are in the grant, and which descriptor
/// has to be given back once they have been read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Frame {
    /// The descriptor and buffer this frame arrived in.
    pub index: usize,
    /// Byte offset of the frame's first byte inside the grant.
    pub at: usize,
    /// Bytes of frame, the Ethernet CRC already stripped by `RCTL.SECRC`.
    pub len: usize,
}

/// A transmit buffer taken out of the ring, before its bytes are written.
///
/// **The buffer is the descriptor's own and the two are taken together**, so
/// nothing is written into a buffer the device is reading: a slot leaves the
/// ring only in [`I219::tx_reserve`] and the descriptor is published only in
/// [`I219::tx_commit`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[must_use = "a slot that is never committed is a transmit descriptor lost for the boot"]
pub struct TxSlot {
    index: usize,
    /// Byte offset of the frame's first byte inside the grant.
    pub at: usize,
    /// Bytes the caller said it would write.
    pub len: usize,
}

/// What one call to [`I219::begin_pass`] found.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Pass {
    /// Messages the claim had taken since the last pass.
    pub messages: u32,
    /// What `ICR` said, before this driver wrote the same bits back.
    pub causes: u32,
    /// Whether the link's state is not what it was at the last pass.
    pub link_changed: bool,
}

/// Everything this driver has refused, dropped or been told about, for the one
/// diagnostic line its caller prints on a change.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Counters {
    /// Frames refused because the device claimed more bytes than the buffer
    /// held.
    pub over_length: u32,
    /// Frames the hardware reported an error on (§7.1.3.4).
    pub errored: u32,
    /// Descriptors written back without `EOP`.
    pub split: u32,
    /// Descriptors written back with nothing in them (§7.1.7.2).
    pub empty: u32,
    /// `ICR.RXO` (§10.2.4.1): the receive FIFO overran.
    pub overruns: u32,
    /// `ICR.RXDMT0`: free descriptors fell to the threshold.
    pub starved: u32,
    /// Frames dropped for want of a free transmit descriptor. A count and not
    /// a wait: a server never blocks, and a dropped frame's recovery is the
    /// peer's retransmit.
    pub tx_dropped: u32,
    /// Messages that arrived with no cause set in `ICR` — §7.4.5's spurious
    /// interrupt, which costs a pass and nothing else.
    pub spurious: u32,
}

/// How long [`I219::open`] waits for `CTRL.RST` to clear itself.
///
/// **A driver-chosen bound, not a datasheet one**: §10.2.2.1 says the bit is
/// self-clearing and gives no time. A hundred milliseconds is far past what
/// any part in reach takes and still a bound, so a window that answers a
/// register file it is not refuses rather than spinning for the boot.
const RESET_DEADLINE_NANOS: u64 = 100_000_000;

/// The function, brought up and driving.
pub struct I219<R, C, D, I> {
    regs: R,
    clock: C,
    dma: D,
    irq: I,
    mac: [u8; 6],
    link: Link,
    /// When [`Self::open`] returned, and when the link first came up — the two
    /// the caller subtracts to get a link-up time.
    opened_at: u64,
    link_up_at: Option<u64>,
    /// Next receive descriptor to look at. Never derived from `RDH`.
    rx_next: usize,
    /// The last value written to `RDT`, which is also the one descriptor the
    /// receive ring keeps in software's hands (§7.1.8: the queue is empty when
    /// head equals tail).
    rx_tail: usize,
    rx_budget: u32,
    /// Next transmit descriptor to hand out, which is also `TDT`.
    tx_next: usize,
    /// Next transmit descriptor to reclaim.
    tx_clean: usize,
    counters: Counters,
}

impl<R: Registers, C: Clock, D: DmaBuffers, I: Interrupts> I219<R, C, D, I> {
    /// Bring the function up in the order §4.6 fixes: reset with every
    /// interrupt masked, the station address, the multicast table, the link,
    /// both rings, then the transmitter, then the receiver, then the interrupt
    /// mask.
    pub fn open(regs: R, clock: C, dma: D, irq: I) -> Result<Self, Refusal> {
        if regs.len() < regs::REGISTER_BYTES {
            return Err(Refusal::Window { given: regs.len(), needed: regs::REGISTER_BYTES });
        }
        if (dma.len() as u64) < GRANT_BYTES {
            return Err(Refusal::Grant { given: dma.len(), needed: GRANT_BYTES as usize });
        }
        // A window nothing decodes answers ones on every access, and a driver
        // that went on would read a MAC address of `ff:ff:ff:ff:ff:ff` out of
        // it and never say why nothing arrived.
        if regs.read(regs::STATUS) == u32::MAX {
            return Err(Refusal::Dead);
        }

        // §4.6.1: interrupts are masked before the reset, so nothing arrives
        // while the register file is being rebuilt. `ICR` is read afterwards
        // because §10.2.4.1's case 1 — mask all — is the one arm that clears
        // it unconditionally.
        regs.write(regs::IMC, u32::MAX);
        let _ = regs.read(regs::ICR);

        // §10.2.2.1: a read-modify-write, because two of this register's
        // reserved bits are documented as "Set to 1b" and one as "must be set
        // to 1b" — a driver that wrote a value it composed itself would clear
        // them.
        let held = regs.read(regs::CTRL);
        regs.write(regs::CTRL, held | ctrl::RST);
        let started = clock.nanos();
        loop {
            if regs.read(regs::CTRL) & ctrl::RST == 0 {
                break;
            }
            let waited = clock.nanos().saturating_sub(started);
            if waited >= RESET_DEADLINE_NANOS {
                return Err(Refusal::ResetUnfinished { after_nanos: waited });
            }
        }
        // Again after the reset: §4.6.1 keeps them masked until the rings
        // exist, and the reset itself is an event the part may have recorded.
        regs.write(regs::IMC, u32::MAX);
        let _ = regs.read(regs::ICR);

        // §10.2.5.23: the reset reloads entry 0 from the NVM, so this is read
        // after it and not before.
        let low = regs.read(regs::RAL0);
        let high = regs.read(regs::RAH0);
        if high & rah::AV == 0 {
            return Err(Refusal::NoStationAddress);
        }
        let mac = [
            low as u8,
            (low >> 8) as u8,
            (low >> 16) as u8,
            (low >> 24) as u8,
            high as u8,
            (high >> 8) as u8,
        ];

        // §4.6.5: "Set up the Multicast Table Array (MTA) per software. This
        // generally means zeroing all entries initially."
        for entry in 0..regs::MTA_DWORDS {
            regs.write(regs::MTA + entry * 4, 0);
        }

        // §10.2.2.1: `SLU` is what lets the MAC see the PHY's link at all;
        // `ASDE` must be zero on this family; forcing speed or duplex would
        // override what auto-negotiation resolved; and this driver negotiates
        // no flow control and strips no VLAN tag.
        let held = regs.read(regs::CTRL);
        let wanted = (held
            & !(ctrl::ASDE | ctrl::ILOS | ctrl::FRCSPD | ctrl::FRCDPLX | ctrl::RFCE | ctrl::TFCE
                | ctrl::VME))
            | ctrl::SLU;
        regs.write(regs::CTRL, wanted);

        // No moderation on either side: §10.2.4.2's throttle and the two
        // receive timers all hold an interrupt back, and what this driver
        // waits on is the frame that has already arrived.
        regs.write(regs::ITR, 0);
        regs.write(regs::RDTR, 0);
        regs.write(regs::RADV, 0);
        regs.write(regs::TIDV, 0);
        regs.write(regs::TADV, 0);

        let mut nic = Self {
            regs,
            clock,
            dma,
            irq,
            mac,
            link: Link::default(),
            opened_at: 0,
            link_up_at: None,
            rx_next: 0,
            rx_tail: RX_RING - 1,
            rx_budget: RX_BUDGET,
            tx_next: 0,
            tx_clean: 0,
            counters: Counters::default(),
        };

        nic.arm_rx_ring();
        nic.arm_tx_ring();

        // §4.6.6, in its order: the write-back policy, the gap, then the
        // transmitter.
        nic.regs.write(regs::TXDCTL, txdctl::SUGGESTED);
        nic.regs.write(regs::TIPG, regs::TIPG_DEFAULT);
        let tx = tctl::EN | tctl::PSP | tctl::CT | tctl::COLD_FULL_DUPLEX;
        nic.regs.write(regs::TCTL, tx);
        nic.accepted(regs::TCTL, tx)?;

        // §4.6.5.1: the receiver last, "only after all other setup is
        // accomplished".
        let rx = rctl::EN | rctl::BAM | rctl::SECRC | rctl::BSIZE_2048;
        nic.regs.write(regs::RCTL, rx);
        nic.accepted(regs::RCTL, rx)?;

        // §4.6.5: and only now the mask, so no cause can arrive before there
        // is a ring to answer it with.
        nic.regs.write(regs::IMS, cause::ENABLED);

        nic.opened_at = nic.clock.nanos();
        nic.refresh_link();
        Ok(nic)
    }

    /// A register wrote what it was told. Read back rather than assumed: the
    /// two registers this checks are the ones that decide whether the part
    /// moves a frame at all, so a window that is not the register file is
    /// refused here instead of looking like a dead network.
    fn accepted(&self, reg: usize, wrote: u32) -> Result<(), Refusal> {
        let read = self.regs.read(reg);
        if read & wrote == wrote {
            Ok(())
        } else {
            Err(Refusal::NotAccepted { reg, wrote, read })
        }
    }

    /// Publish every receive descriptor and hand the ring to the device.
    ///
    /// §4.6.5.1: base, length, head, then "the tail pointer should be set to
    /// point one descriptor beyond the end" — so the ring holds `RX_RING`
    /// buffers and `RX_RING - 1` of them are the device's at any moment, the
    /// last being the one §7.1.8's "head equals tail is empty" costs.
    fn arm_rx_ring(&mut self) {
        for index in 0..RX_RING {
            self.publish_rx(index);
        }
        let base = self.dma.device_addr(OFF_RX_RING);
        self.dma.publish();
        self.regs.write(regs::RDBAL, base as u32);
        self.regs.write(regs::RDBAH, (base >> 32) as u32);
        self.regs.write(regs::RDLEN, (RX_RING * rx_desc::BYTES) as u32);
        self.regs.write(regs::RDH, 0);
        self.regs.write(regs::RDT, self.rx_tail as u32);
    }

    fn arm_tx_ring(&mut self) {
        for index in 0..TX_RING {
            let at = OFF_TX_RING + index * tx_desc::BYTES;
            self.dma.write(at, 0);
            self.dma.write(at + 8, 0);
        }
        let base = self.dma.device_addr(OFF_TX_RING);
        self.dma.publish();
        self.regs.write(regs::TDBAL, base as u32);
        self.regs.write(regs::TDBAH, (base >> 32) as u32);
        self.regs.write(regs::TDLEN, (TX_RING * tx_desc::BYTES) as u32);
        self.regs.write(regs::TDH, 0);
        self.regs.write(regs::TDT, 0);
    }

    /// Write descriptor `index` back as an empty buffer the device may fill.
    ///
    /// The status word is zeroed before the address is published, because
    /// §7.1.8 says software can "zero the status byte in the descriptor to
    /// make it ready for reuse" and a stale `DD` left behind would be read as
    /// a completion for a frame that never arrived.
    fn publish_rx(&mut self, index: usize) {
        let at = OFF_RX_RING + index * rx_desc::BYTES;
        let buffer = OFF_RX_BUFS + index * RX_BUF_BYTES;
        self.dma.write(at + 8, 0);
        self.dma.write(at, self.dma.device_addr(buffer));
    }

    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }

    pub fn link(&self) -> Link {
        self.link
    }

    pub fn counters(&self) -> Counters {
        self.counters
    }

    /// Nanoseconds between the function coming up and its link doing so, once
    /// it has. `None` while the link has never been up.
    pub fn link_up_after_nanos(&self) -> Option<u64> {
        self.link_up_at.map(|at| at.saturating_sub(self.opened_at))
    }

    /// Take the interrupt, read and acknowledge its causes, and give the
    /// receive budget back.
    ///
    /// **The record has to be taken, not merely noticed**: a claim reads ready
    /// while it holds an undrained interrupt, so a caller that saw the token
    /// and left it would find the same one on every later wait.
    pub fn begin_pass(&mut self) -> Pass {
        self.rx_budget = RX_BUDGET;
        let messages = self.irq.taken();

        // Once, and written back. §10.2.4.1's case 3 says a read with no
        // interrupt asserted has no side effect at all, so a driver that
        // treated the read as the acknowledgement would see the same causes
        // for ever; and §7.4.5's "Write to Clear" is the arm that always
        // works. `INT_ASSERTED` is left out because the same section says
        // writing it has no effect and it clears when its causes do.
        let causes = self.regs.read(regs::ICR);
        let acknowledged = causes & !cause::INT_ASSERTED;
        if acknowledged != 0 {
            self.regs.write(regs::ICR, acknowledged);
        }

        if causes & cause::RXO != 0 {
            self.counters.overruns = self.counters.overruns.saturating_add(1);
        }
        if causes & cause::RXDMT0 != 0 {
            self.counters.starved = self.counters.starved.saturating_add(1);
        }
        if messages > 0 && acknowledged == 0 {
            self.counters.spurious = self.counters.spurious.saturating_add(messages);
        }

        // On `LSC` and on every pass that found no cause at all: the link can
        // also come up before the mask was written, and then no `LSC` is ever
        // delivered for it.
        let before = self.link;
        if causes & cause::LSC != 0 || !self.link.up {
            self.refresh_link();
        }
        Pass { messages, causes, link_changed: self.link != before }
    }

    /// Re-read `STATUS` and take what it says about the link.
    fn refresh_link(&mut self) {
        let status = self.regs.read(regs::STATUS);
        let up = status & status::LU != 0;
        // §10.2.2.2: `11b` is 1000 Mb/s too, so this is not a lookup that can
        // answer "unknown".
        let speed = match (status >> status::SPEED_SHIFT) & status::SPEED_MASK {
            0b00 => 10,
            0b01 => 100,
            _ => 1000,
        };
        self.link = Link {
            up,
            speed_mbps: if up { speed } else { 0 },
            full_duplex: status & status::FD != 0,
        };
        if up && self.link_up_at.is_none() {
            self.link_up_at = Some(self.clock.nanos());
        }
    }

    /// The next received frame, or `None` — for an empty ring and for a budget
    /// already spent alike.
    ///
    /// A refused descriptor is counted, its buffer given straight back, and
    /// the walk continues: one frame this driver will not act on may not hide
    /// the ones behind it.
    pub fn poll_rx(&mut self) -> Option<Frame> {
        loop {
            if self.rx_budget == 0 {
                return None;
            }
            let index = self.rx_next;
            let at = OFF_RX_RING + index * rx_desc::BYTES;
            let word = self.dma.read(at + 8);
            let status = ((word >> rx_desc::STATUS_SHIFT) & rx_desc::BYTE_MASK) as u8;
            if status & rx_desc::status::DD == 0 {
                return None;
            }
            // §7.1.7.1: the descriptor was written back in a batch, so its
            // other fields are read only after the load that found `DD`.
            self.dma.observe();
            let word = self.dma.read(at + 8);
            self.rx_next = (index + 1) % RX_RING;
            self.rx_budget -= 1;
            match parse_rx(word) {
                Ok(len) => {
                    return Some(Frame {
                        index,
                        at: OFF_RX_BUFS + index * RX_BUF_BYTES,
                        len,
                    })
                }
                Err(why) => {
                    match why {
                        RxRefusal::Length { .. } => {
                            self.counters.over_length = self.counters.over_length.saturating_add(1)
                        }
                        RxRefusal::Errors { .. } => {
                            self.counters.errored = self.counters.errored.saturating_add(1)
                        }
                        RxRefusal::Split => {
                            self.counters.split = self.counters.split.saturating_add(1)
                        }
                        RxRefusal::Empty { .. } => {
                            self.counters.empty = self.counters.empty.saturating_add(1)
                        }
                    }
                    self.rx_done(index);
                }
            }
        }
    }

    /// Give buffer `index` back to the device, its frame having been read.
    ///
    /// **Buffers come back in the order they were handed out**, which is what
    /// makes one `RDT` write enough: §7.1.8's tail "identifies the location
    /// beyond the last descriptor hardware can process", so it can only ever
    /// be advanced over a run of descriptors that are all ready. A buffer
    /// never given back is a receive slot lost for the boot.
    pub fn rx_done(&mut self, index: usize) {
        let expected = (self.rx_tail + 1) % RX_RING;
        assert!(
            index == expected,
            "toyos-i219: buffer {index} came back out of turn; {expected} is the one the \
             receive tail can move over"
        );
        self.publish_rx(index);
        self.rx_tail = index;
        // §7.1.8: the device fetches on the tail write, so the descriptor has
        // to be there before the write is.
        self.dma.publish();
        self.regs.write(regs::RDT, index as u32);
    }

    /// Take a transmit descriptor and its buffer, or `None` where every one is
    /// in flight.
    ///
    /// Non-blocking by construction. A caller with no slot drops the frame:
    /// spinning on the ring here would park its whole event loop on a device.
    pub fn tx_reserve(&mut self, len: usize) -> Option<TxSlot> {
        assert!(
            len <= TX_BUF_BYTES,
            "toyos-i219: a {len}-byte frame does not fit a {TX_BUF_BYTES}-byte buffer"
        );
        self.reclaim_tx();
        // §7.2.4: hardware owns `[TDH..TDT)`, so a ring filled to the last
        // descriptor would wrap the tail onto the head and read as empty.
        if (self.tx_next + 1) % TX_RING == self.tx_clean {
            self.counters.tx_dropped = self.counters.tx_dropped.saturating_add(1);
            return None;
        }
        let index = self.tx_next;
        Some(TxSlot { index, at: OFF_TX_BUFS + index * TX_BUF_BYTES, len })
    }

    /// Publish the frame in `slot` and ring the doorbell.
    pub fn tx_commit(&mut self, slot: TxSlot) {
        let at = OFF_TX_RING + slot.index * tx_desc::BYTES;
        let command = (tx_desc::cmd::ONE_FRAME as u64) << tx_desc::CMD_SHIFT;
        // The status nibble is zeroed with the rest of the word, so the `DD`
        // this driver waits for is one the device wrote.
        self.dma.write(at + 8, (slot.len as u64 & tx_desc::LENGTH_MASK) | command);
        self.dma.write(at, self.dma.device_addr(slot.at));
        self.tx_next = (slot.index + 1) % TX_RING;
        // §7.2.4.1: "The 82574 NEVER fetches descriptors beyond the descriptor
        // tail pointer", so the descriptor and the frame both have to be
        // visible before the tail moves over them.
        self.dma.publish();
        self.regs.write(regs::TDT, self.tx_next as u32);
    }

    /// Take back every transmit descriptor the device has written back.
    ///
    /// From the oldest, one at a time, stopping at the first `DD` that is
    /// clear: §7.2.4.2 lets the device write descriptors back in batches, so
    /// a later descriptor's `DD` says nothing about an earlier one's.
    fn reclaim_tx(&mut self) {
        while self.tx_clean != self.tx_next {
            let at = OFF_TX_RING + self.tx_clean * tx_desc::BYTES;
            let word = self.dma.read(at + 8);
            let status = ((word >> tx_desc::STATUS_SHIFT) & tx_desc::STATUS_MASK) as u8;
            if status & tx_desc::STATUS_DD == 0 {
                return;
            }
            self.dma.observe();
            self.tx_clean = (self.tx_clean + 1) % TX_RING;
        }
    }

    /// Transmit descriptors nothing is in flight on.
    pub fn tx_free(&self) -> usize {
        TX_RING - 1 - (self.tx_next + TX_RING - self.tx_clean) % TX_RING
    }
}

/// What a written-back receive descriptor must satisfy, as bit arithmetic on
/// the one word the device wrote.
///
/// Separate from the volatile reads so the tests can drive it with words no
/// working part would write. Each arm is a way to make this driver act on a
/// number the device chose.
pub fn parse_rx(word: u64) -> Result<usize, RxRefusal> {
    let len = ((word >> rx_desc::LENGTH_SHIFT) & rx_desc::LENGTH_MASK) as u32;
    let status = ((word >> rx_desc::STATUS_SHIFT) & rx_desc::BYTE_MASK) as u8;
    let errors = ((word >> rx_desc::ERRORS_SHIFT) & rx_desc::BYTE_MASK) as u8;
    // §7.1.3.3: with `EOP` clear "only the Address, Length, and DD bits are
    // valid", so the length is not trusted and the errors are not read.
    if status & rx_desc::status::EOP == 0 {
        return Err(RxRefusal::Split);
    }
    if errors & rx_desc::errors::FRAME_IS_BAD != 0 {
        return Err(RxRefusal::Errors { errors });
    }
    // Before the length is believed and not after: the bound is what keeps
    // this number from becoming a read past the buffer.
    if len as usize > RX_BUF_BYTES {
        return Err(RxRefusal::Length { len });
    }
    // A frame shorter than its own header is nothing to hand up, and
    // §7.1.7.2's null descriptor padding lands here with a length of zero.
    if (len as usize) < ETHERNET_HEADER_BYTES {
        return Err(RxRefusal::Empty { len });
    }
    Ok(len as usize)
}

/// Destination, source and EtherType: the shortest thing that can be a frame.
const ETHERNET_HEADER_BYTES: usize = 14;
