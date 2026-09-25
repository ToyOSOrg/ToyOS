//! The Intel I219's driver — every decision it makes, and none of the
//! instructions that carry them out.
//!
//! The part is the T14's onboard `8086:15fc` at `00:1f.6`, or the `8086:10d3`
//! that QEMU's `e1000e` models. Every `§` in this file and in [`regs`] is a
//! section of the *Intel 82574 GbE Controller Family Datasheet* (317694-018,
//! rev 2.7).
//!
//! **Below the register file the two parts are not one.** §3.2.1 puts the
//! 82574's PHY on the controller's own die; the T14's is a MAC in the PCH whose
//! PHY is separate silicon the Management Engine shares, reached over `MDIC`
//! under §4.5.2's ownership arbitration — which this driver asks for, waits a
//! bounded time on, and gives back. [`Part`] is which one this claim is, and
//! [`phy`] is everything that follows from it. [`wake`] is what brings that PHY
//! within `MDIC`'s reach before the reset and what shape the reset then takes —
//! the MAC and the PHY together — on properties of the part no Intel document
//! publishes, and `pch` is what the PCH's MAC is given before its rings.
//! [`lease`] is the one table a lease probe's answer crosses a machine with no
//! console through.
//!
//! # The boundary
//!
//! Four traits, named by what they do and not by what chip is behind them:
//! [`Registers`] is one memory-mapped access, [`Clock`] is one counter read,
//! [`DmaBuffers`] is the memory the device and this process share, and
//! [`Interrupts`] is what the claim answered. Each is a generic parameter
//! resolved at compile time — no trait object, no dispatch — and no
//! implementation of one decides anything: every branch is above the boundary,
//! in this crate, where the host tests it.
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
//! given becomes the length of a slice netd hands to smoltcp, so `parse_rx`
//! bounds it, and `RxRefusal` is every way a written-back descriptor is refused
//! rather than believed.
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
//! - **`IVAR` is written and read back, and a part that does not take it is
//!   refused.** §10.2.4.9 says the register "is only valid in MSI-X mode" and
//!   allocates no cause to a vector at reset; what a part outside that mode
//!   answers instead is not in the document, so it is refused by name rather
//!   than guessed at.

#![no_std]
#![forbid(unsafe_code)]

#[cfg(test)]
extern crate std;

pub mod lease;
mod pch;
pub mod phy;
pub mod power;
pub mod regs;
pub mod wake;

#[cfg(test)]
mod stub;
#[cfg(test)]
mod tests;

use regs::{cause, ctrl, ivar, rah, rctl, rx_desc, status, tctl, tx_desc, txdctl};

/// One memory-mapped register access.
///
/// **The offsets are this crate's own constants**, every one of them under
/// [`regs::REGISTER_BYTES`], which [`I219::open`] refuses a smaller window
/// than.
pub trait Registers {
    /// Bytes the window covers.
    fn bytes(&self) -> usize;
    /// One volatile 32-bit load. Volatile because the device writes the same
    /// bytes, and a plain load of a status register can be hoisted out of the
    /// loop that waits on it.
    fn read(&self, reg: usize) -> u32;
    /// One volatile 32-bit store.
    fn write(&self, reg: usize, value: u32);
}

/// The clock, and the one way this driver gives the processor away.
pub trait Clock {
    /// One counter read: nanoseconds on a clock that does not go backwards.
    fn nanos(&self) -> u64;
    /// Give the processor away for about `nanos`.
    ///
    /// **It decides nothing**: every bound and every pace in this crate is read
    /// off [`Self::nanos`], so a pause that returns early or late moves when a
    /// register is next reached and never what a reading of one means. A
    /// substrate with nothing to yield to may return at once.
    fn pause(&self, nanos: u64);
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
    /// Bytes in the grant.
    fn bytes(&self) -> usize;
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

/// What the claim answered when its interrupt record was read.
///
/// **The implementation hands the answer over and decides nothing about it**:
/// which refusal means "nothing arrived" and which means the function is no
/// longer this driver's is [`I219::begin_pass`]'s call, made once and tested on
/// the host.
pub trait Interrupts {
    /// The word the substrate refuses with.
    type Refused: PartialEq + core::fmt::Debug;
    /// The one refusal that is ordinary: nothing since the last read.
    const IDLE: Self::Refused;
    /// Messages that have arrived since the last call. The count is never what
    /// a message *meant* — that is in `ICR` and in the rings.
    fn taken(&self) -> Result<u32, Self::Refused>;
}

/// How many receive descriptors the ring holds. §7.1.8: `RDLEN` "must be a
/// multiple of 128", so the count is a multiple of eight.
pub const RX_RING: usize = 256;
/// How many transmit descriptors the ring holds. A frame is handed to the
/// device the moment it is filled and reclaimed on the next send, so what this
/// bounds is how many may be in flight at once.
pub const TX_RING: usize = 16;
/// Bytes per receive buffer — `RCTL.BSIZE = 00b` with `BSEX` clear (§10.2.5.1).
pub const RX_BUF_BYTES: usize = 2048;
/// Bytes per transmit buffer, and therefore the longest frame [`I219::tx_reserve`]
/// takes.
pub const TX_BUF_BYTES: usize = 2048;

const _: () = {
    // §7.1.8 and §7.2.4: both ring lengths are programmed in bytes and "must be
    // a multiple of 128".
    assert!((RX_RING * rx_desc::BYTES).is_multiple_of(128));
    assert!((TX_RING * tx_desc::BYTES).is_multiple_of(128));
    // §10.2.5.8 and §10.2.6.7: head and tail are sixteen-bit descriptor
    // indices, so a ring longer than that could not be pointed at.
    assert!(RX_RING <= u16::MAX as usize);
    assert!(TX_RING <= u16::MAX as usize);
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
    assert!(OFF_RX_RING.is_multiple_of(16) && OFF_TX_RING.is_multiple_of(16));
};

/// Frames this driver hands up in one pass before it says there are no more.
///
/// **A ring that never empties is a ring the specification permits**: at line
/// rate the device refills descriptors as fast as they are returned, and a
/// caller that loops until the ring is empty never returns to its other work.
pub const RX_BUDGET: u32 = 64;

/// Why the function was not brought up. Each keeps its own word: a caller asks
/// different things of a grant that is too small and of a part with no station
/// address.
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
pub(crate) enum RxRefusal {
    /// More bytes than the buffer the descriptor was given. The one that
    /// matters most: this number becomes the length of a slice.
    Length { len: u32 },
    /// §7.1.3.4's error bits, valid because `EOP` and `DD` are both set.
    Errors { errors: u8 },
    /// `DD` without `EOP`: no frame on this link can span two 2048-byte
    /// buffers, so a device that split one is not answering the configuration
    /// it was given.
    Split,
    /// Nothing in it — §7.1.7.2's null descriptor padding, or a frame shorter
    /// than an Ethernet header.
    Empty { len: u32 },
}

/// What the link is doing, as `STATUS` last answered.
///
/// **Speed and duplex live inside the up arm**, because §10.2.2.2 makes them a
/// resolution and a link that is down resolved nothing: a driver that carried
/// the last speed across a link going away would report a number the part is no
/// longer standing behind.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Link {
    #[default]
    Down,
    Up {
        speed: Speed,
        full_duplex: bool,
    },
}

impl Link {
    pub fn is_up(self) -> bool {
        matches!(self, Self::Up { .. })
    }
}

/// What §10.2.2.2's `STATUS.SPEED` resolved to.
///
/// **Three of them and not a number**: the field is two bits and `11b` is
/// 1000 Mb/s as well, so every value it can hold is one of these and nothing
/// downstream has an "unknown speed" arm to write.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Speed {
    Mbps10,
    Mbps100,
    #[default]
    Mbps1000,
}

impl Speed {
    fn in_status(status: u32) -> Self {
        match (status >> status::SPEED_SHIFT) & status::SPEED_MASK {
            0b00 => Self::Mbps10,
            0b01 => Self::Mbps100,
            _ => Self::Mbps1000,
        }
    }

    pub fn mbps(self) -> u16 {
        match self {
            Self::Mbps10 => 10,
            Self::Mbps100 => 100,
            Self::Mbps1000 => 1000,
        }
    }
}

/// One received frame, and the receipt for the buffer it arrived in.
///
/// **There is no index a caller can get wrong**: the descriptor this stands for
/// is the frame's own, [`I219::rx_done`] takes the receipt by value, and
/// nothing outside this crate can make a second one. Several may be held at
/// once and given back in any order; what the order decides is when the tail
/// moves, not which buffer is returned.
#[must_use = "a frame whose buffer is never given back is a receive slot lost for the boot"]
#[derive(PartialEq, Eq, Debug)]
pub struct Frame {
    index: usize,
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
    /// Frames the caller offered that no transmit buffer holds.
    pub too_long: u32,
    /// Frames dropped for want of a free transmit descriptor. A count and not
    /// a wait: a server never blocks, and a dropped frame's recovery is the
    /// peer's retransmit.
    pub tx_dropped: u32,
    /// Messages that arrived with no cause set in `ICR` — §7.4.5's spurious
    /// interrupt. It costs a pass and nothing else.
    pub spurious: u32,
    /// Frames the part took off the transmit ring and wrote `DD` back for.
    pub sent: u32,
    /// Frames the part filled a receive descriptor with and this driver handed
    /// up.
    pub received: u32,
}

impl Counters {
    /// The counts that are worth a line: every one but [`Self::spurious`],
    /// [`Self::sent`] and [`Self::received`], which move on their own on a
    /// working card, so a diagnostic keyed on them would print on nothing
    /// having gone wrong.
    pub fn anomalies(&self) -> Self {
        Self { spurious: 0, sent: 0, received: 0, ..*self }
    }
}

/// What the MAC's own statistics registers counted, as [`I219::wire`] adds
/// them up.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Wire {
    /// Good packets transmitted: frames the MAC put on the wire.
    pub sent: u64,
    /// Good packets received: frames that passed its filters.
    pub received: u64,
    /// Total packets received: every frame the MAC saw arrive.
    pub seen: u64,
    /// Frames it had no room for.
    pub missed: u64,
    /// Frames that arrived with a bad CRC.
    pub crc_errors: u64,
}

impl Wire {
    fn plus(self, more: Self) -> Self {
        Self {
            sent: self.sent.saturating_add(more.sent),
            received: self.received.saturating_add(more.received),
            seen: self.seen.saturating_add(more.seen),
            missed: self.missed.saturating_add(more.missed),
            crc_errors: self.crc_errors.saturating_add(more.crc_errors),
        }
    }
}

/// How long [`I219::open`] waits for `CTRL.RST` to clear itself.
///
/// **A driver-chosen bound, not a datasheet one**: §10.2.2.1 says the bit is
/// self-clearing and gives no time. A hundred milliseconds is far past what
/// any part in reach takes and still a bound, so a window that answers a
/// register file it is not refuses rather than spinning for the boot.
const RESET_DEADLINE_NANOS: u64 = 100_000_000;

/// How long [`I219::open`] leaves `CTRL.RST` alone after writing it.
///
/// §10.2.2.1: "designers must wait approximately 1 µs after resetting before
/// attempting to check to see if the bit has cleared or attempting to access
/// (read or write) any other device register."
const RESET_SETTLE_NANOS: u64 = 1_000;

/// How long [`I219::open`] waits for §3.1.3.10's master quiesce.
///
/// **A driver-chosen bound, not a datasheet one**: §3.1.3.10 describes the
/// handshake and says only that "the software device driver might time out if
/// the PCIe Master Enable Status bit is not cleared within a given time".
const MASTER_QUIESCE_DEADLINE_NANOS: u64 = 10_000_000;

/// The transmit control [`I219::open`] writes: §4.6.6's suggested values with
/// the transmitter enabled.
const TX_CONTROL: u32 = tctl::EN | tctl::PSP | tctl::CT | tctl::COLD_FULL_DUPLEX;

/// What [`reset`] leaves its caller.
struct Reset {
    /// Whether §3.1.3.10's master quiesce finished before the reset was issued.
    master_quiet: bool,
    /// When `CTRL.RST` was written, which §10.2.2.1's two bounds and §9.2's
    /// delay before the first MDIO access are measured from.
    at: u64,
    /// What the I219's full reset did, and `None` for a reset of the MAC alone.
    full: Option<wake::FullReset>,
    /// When the flag the full reset was issued under was given back, which the
    /// next hold's pace runs from.
    released: Option<u64>,
}

/// Which reset [`reset`] issues.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Whole {
    /// §10.2.2.1's `RST` alone: the 82574, whose PHY is on its own die.
    MacAlone,
    /// The PCH's MAC and its PHY together, as [`wake`]'s header sets out,
    /// under §8.2.4's flag taken no nearer than one pace to `after` — when an
    /// earlier hold of it was given back.
    WithThePhy { after: Option<u64> },
}

/// Stop what a previous holder left the part doing, before this driver's first
/// grant lets it master the bus again: every interrupt masked, receive and
/// transmit off.
///
/// **The function keeps its rings across a release**, and the grant is the
/// instant it may use them — an enabled receive unit would write the next
/// frame into the previous holder's descriptors. The kernel resets a released
/// function where it advertises a reset; this is the driver not trusting that
/// it did. Register writes only, so it is safe with mastering off.
pub fn quiesce<R: Registers>(regs: &R) {
    regs.write(regs::IMC, u32::MAX);
    let rctl = regs.read(regs::RCTL);
    regs.write(regs::RCTL, rctl & !rctl::EN);
    let tctl = regs.read(regs::TCTL);
    regs.write(regs::TCTL, tctl & !tctl::EN);
}

/// §4.6.1's reset, with every interrupt masked on both sides of it.
fn reset<R: Registers, C: Clock>(
    regs: &R,
    clock: &C,
    whole: Whole,
) -> Result<Reset, Refusal> {
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

    // §10.2.2.1: "Before issuing this reset, software has to insure that Tx
    // and Rx processes are stopped by following the procedure described in
    // Section 3.1.3.10" — §3.1.3.10's master disable, which is how a
    // function firmware left driving stops reaching memory before its rings
    // are rebuilt under it. The expiry is not a refusal because the same
    // section sanctions it: "the software device driver might time out if
    // the PCIe Master Enable Status bit is not cleared within a given
    // time", and the reset clears the bit either way.
    let held = regs.read(regs::CTRL);
    regs.write(regs::CTRL, held | ctrl::GIO_MASTER_DISABLE);
    let started = clock.nanos();
    let master_quiet = loop {
        if regs.read(regs::STATUS) & status::GIO_MASTER_ENABLE == 0 {
            break true;
        }
        if clock.nanos().saturating_sub(started) >= MASTER_QUIESCE_DEADLINE_NANOS {
            break false;
        }
    };

    // §10.2.2.1: a read-modify-write, because two of this register's
    // reserved bits are documented as "Set to 1b" and one as "must be set
    // to 1b" — a driver that wrote a value it composed itself would clear
    // them.
    let held = regs.read(regs::CTRL);
    // Held until the reset has finished and the PHY's configuration with it:
    // the drop is paced from the reset's own write, and the waits below are
    // measured from that write too.
    let mut flag = None;
    let (reset_at, full) = match whole {
        Whole::MacAlone => {
            regs.write(regs::CTRL, held | ctrl::RST);
            // The write is the event §10.2.2.1's two bounds and §9.2's delay
            // before the first MDIO access are measured from.
            let reset_at = clock.nanos();
            // The settle is a wait and not a poll: §10.2.2.1 owes the
            // microsecond to "attempting to check to see if the bit has
            // cleared or attempting to access (read or write) any other
            // device register" alike, so there is nothing this driver may read
            // to shorten it.
            while clock.nanos().saturating_sub(reset_at) < RESET_SETTLE_NANOS {}
            (reset_at, None)
        }
        Whole::WithThePhy { after } => {
            let firmware = regs.read(regs::FWSM);
            let word = wake::reset_word(held, firmware);
            // Under §8.2.4's flag, which arbitrates the CSRs this MAC shares
            // with its firmware — and a flag that would not come is no reason
            // to leave the part unreset, so the write goes out either way. The reset
            // clears the flag, and the drop below gives back one it left
            // standing.
            let claimed = phy::Owned::claim(regs, clock, after);
            match &claimed {
                Ok(mdi) => mdi.write_paced(regs::CTRL, word),
                Err(_) => regs.write(regs::CTRL, word),
            }
            let reset_at = clock.nanos();
            // Nothing is touched while the part resets both ends of its
            // interconnect ([`wake`]'s header): a wait and not a poll.
            phy::hold(clock, reset_at, wake::RESET_QUIET_NANOS);
            let full = wake::FullReset {
                phy_reset: word & ctrl::PHY_RST != 0,
                flag: claimed.as_ref().map(|_| ()).map_err(|why| *why),
                configured_after_nanos: None,
            };
            flag = Some(claimed);
            (reset_at, Some(full))
        }
    };
    loop {
        if regs.read(regs::CTRL) & ctrl::RST == 0 {
            break;
        }
        let waited = clock.nanos().saturating_sub(reset_at);
        if waited >= RESET_DEADLINE_NANOS {
            return Err(Refusal::ResetUnfinished { after_nanos: waited });
        }
    }
    // The configuration the MAC gives the PHY after a reset that reached it,
    // waited on and never refused on: I219 Table 5-2 bounds it, and the PHY's
    // own answer to the bring-up is the test of whether it finished.
    let full = full.map(|full| wake::FullReset {
        configured_after_nanos: full.phy_reset.then(|| phy_configured(regs, clock, reset_at)).flatten(),
        ..full
    });
    let released = flag.map(|flag| {
        drop(flag);
        clock.nanos()
    });
    // Again after the reset: §4.6.1 keeps them masked until the rings
    // exist, and the reset itself is an event the part may have recorded.
    regs.write(regs::IMC, u32::MAX);
    let _ = regs.read(regs::ICR);
    Ok(Reset { master_quiet, at: reset_at, full, released })
}

/// How long after `since` `STATUS` said the PHY was configured, or `None`
/// where [`wake::PHY_CONFIGURED_DEADLINE_NANOS`] ran out first.
fn phy_configured<R: Registers, C: Clock>(regs: &R, clock: &C, since: u64) -> Option<u64> {
    loop {
        let done = regs.read(regs::STATUS) & status::PHY_CONFIGURED != 0;
        let waited = clock.nanos().saturating_sub(since);
        if done {
            return Some(waited);
        }
        if waited >= wake::PHY_CONFIGURED_DEADLINE_NANOS {
            return None;
        }
        clock.pause(wake::PHY_CONFIGURED_PACE_NANOS);
    }
}

/// Which part the claim is on.
///
/// **The parent's answer and never a probe**: `/system/bin/init` moved a claim
/// on a declared vendor and device into this process, and a driver that read
/// the register file to work out which part it was on would be guessing at the
/// registers it does not yet trust.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Part {
    /// The 82574, whose PHY §3.2.1 puts on the controller's own die.
    E82574,
    /// The ThinkPad T14's `8086:15fc`: a MAC in the PCH whose PHY is the
    /// separate silicon the *Intel Ethernet Connection I219 Datasheet*
    /// describes and the Management Engine shares.
    I219,
}

/// What [`I219::open`] found on the way up, for the one line a caller prints
/// about a function that raised no link.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BringUp {
    /// Whether §3.1.3.10's master quiesce finished before the reset was issued.
    pub master_quiet: bool,
    /// What [`phy::wake`] asked the I219's PHY before the reset, rung by rung,
    /// or why it could not ask; `None` on the 82574.
    pub woke: Option<Result<wake::Woke, phy::PhyRefusal>>,
    /// What the I219's full reset did; `None` on the 82574.
    pub reset: Option<wake::FullReset>,
    /// What the PHY answered, or why it was not reached.
    pub phy: Result<phy::Phy, phy::PhyRefusal>,
}

/// The function, brought up and driving.
///
/// **Dropping it lets the function go**: on the I219, [`pch::release`] takes
/// back the word [`pch::prepare`] gave the firmware, on every way a holder
/// leaves that runs its destructors.
pub struct I219<R: Registers, C, D, I> {
    part: Part,
    regs: R,
    clock: C,
    dma: D,
    irq: I,
    mac: [u8; 6],
    brought_up: BringUp,
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
    /// What [`Self::wire`] has read out of the statistics registers so far.
    wire: Wire,
}

impl<R: Registers, C, D, I> Drop for I219<R, C, D, I> {
    fn drop(&mut self) {
        if self.part == Part::I219 {
            pch::release(&self.regs);
        }
    }
}

impl<R: Registers, C: Clock, D: DmaBuffers, I: Interrupts> I219<R, C, D, I> {
    /// Bring the function up in the order §4.6 fixes: reset with every
    /// interrupt masked, the station address, the multicast table, the PHY, the
    /// link, both rings, then the transmitter, then the receiver, then the
    /// interrupt mask. On the I219 the PHY is brought within reach first and
    /// the reset takes it with the MAC ([`wake`]).
    ///
    /// **The PHY is between the multicast table and `CTRL.SLU`**, because
    /// §4.6.3.2 makes `STATUS.LU` the MAC's report of a link "from the PHY
    /// qualified with CTRL.SLU": a driver that let the MAC look before the PHY
    /// was configured would read the answer to the wrong question.
    pub fn open(part: Part, regs: R, clock: C, dma: D, irq: I) -> Result<Self, Refusal> {
        if regs.bytes() < regs::REGISTER_BYTES {
            return Err(Refusal::Window { given: regs.bytes(), needed: regs::REGISTER_BYTES });
        }
        if (dma.bytes() as u64) < GRANT_BYTES {
            return Err(Refusal::Grant { given: dma.bytes(), needed: GRANT_BYTES as usize });
        }
        // The PHY is brought within reach before the reset, and the reset
        // takes it with the MAC: [`wake`]'s header is why, on the part whose
        // PHY is not on the MAC's own die.
        let (woke, whole) = match part {
            Part::I219 => {
                let woke = phy::wake(&regs, &clock, None);
                (Some(woke), Whole::WithThePhy { after: Some(clock.nanos()) })
            }
            Part::E82574 => (None, Whole::MacAlone),
        };
        let Reset { master_quiet, at: reset_at, full, released } =
            reset(&regs, &clock, whole)?;

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

        // What the PCH's MAC is given after its reset and before its rings:
        // [`pch`]'s header. After the last refusal that comes before `Self`
        // exists, so every refusal after it is a drop that lets the function go.
        if part == Part::I219 {
            pch::prepare(&regs);
        }

        // §4.6.5: "Set up the Multicast Table Array (MTA) per software. This
        // generally means zeroing all entries initially." The table this part
        // has, and nothing past it.
        let table = match part {
            Part::E82574 => regs::MTA_DWORDS,
            Part::I219 => regs::MTA_DWORDS_PCH,
        };
        for entry in 0..table {
            regs.write(regs::MTA + entry * 4, 0);
        }

        // §4.6.3.1: "Refer to the PHY documentation for the initialization and
        // link setup steps. The device driver uses the MDIC register to
        // initialize the PHY and setup the link." The PHY documentation this
        // driver has is the I219's, and §10.2.2.7 addresses the 82574's own PHY
        // under a scheme of its own — so the sequence is refused by name on the
        // part it was not written from.
        let phy = match part {
            Part::I219 => phy::bring_up(&regs, &clock, reset_at, released),
            Part::E82574 => Err(phy::PhyRefusal::NotThisRegisterMap),
        };

        // §10.2.2.1: `SLU` is what lets the MAC see the PHY's link at all;
        // `ASDE` must be zero on this family; forcing speed or duplex would
        // override what auto-negotiation resolved; and this driver negotiates
        // no flow control and strips no VLAN tag. §3.1.3.10's master disable
        // goes with them and is not preserved: a part that came out of the
        // reset still blocking master requests would fetch no descriptor and
        // write back no frame, and nothing else in this bring-up would say so.
        let held = regs.read(regs::CTRL);
        let wanted = (held
            & !(ctrl::GIO_MASTER_DISABLE
                | ctrl::PHY_RST
                | ctrl::ASDE
                | ctrl::ILOS
                | ctrl::FRCSPD
                | ctrl::FRCDPLX
                | ctrl::RFCE
                | ctrl::TFCE
                | ctrl::VME))
            | ctrl::SLU;
        regs.write(regs::CTRL, wanted);

        // §10.2.4.7: "If any bits are set in EIAC, the ICR register should not
        // be read" — and this driver reads it, so auto-clear stays off and
        // every cause is acknowledged by the write-back in `begin_pass`.
        regs.write(regs::EIAC, 0);

        let mut nic = Self {
            part,
            regs,
            clock,
            dma,
            irq,
            mac,
            brought_up: BringUp { master_quiet, woke, reset: full, phy },
            link: Link::default(),
            opened_at: 0,
            link_up_at: None,
            rx_next: 0,
            rx_tail: RX_RING - 1,
            rx_budget: RX_BUDGET,
            tx_next: 0,
            tx_clean: 0,
            counters: Counters::default(),
            wire: Wire::default(),
        };

        // §10.2.4.9: `IVAR` allocates every cause to no vector at reset, so a
        // part in MSI-X mode with it unprogrammed fills `ICR` and delivers
        // nothing. Read back, because the document defines the register only
        // "in MSI-X mode" and says nothing about what a part outside that mode
        // answers — so a part that does not take the write is refused here
        // rather than driven on a guess about which interrupt it would raise.
        nic.regs.write(regs::IVAR, ivar::ALL_ON_VECTOR_ZERO);
        nic.accepted(regs::IVAR, ivar::ALL_ON_VECTOR_ZERO)?;

        // No moderation on either side: §10.2.4.2's throttle and the two
        // receive timers all hold an interrupt back, and what this driver
        // waits on is the frame that has already arrived.
        nic.regs.write(regs::ITR, 0);
        nic.regs.write(regs::RDTR, 0);
        nic.regs.write(regs::RADV, 0);
        nic.regs.write(regs::TIDV, 0);
        nic.regs.write(regs::TADV, 0);

        nic.arm_rx_ring();
        nic.arm_tx_ring();

        // §4.6.6, in its order: the write-back policy, the gap, then the
        // transmitter.
        nic.regs.write(regs::TXDCTL, txdctl::SUGGESTED);
        nic.regs.write(regs::TIPG, regs::TIPG_DEFAULT);
        nic.regs.write(regs::TCTL, TX_CONTROL);
        nic.accepted(regs::TCTL, TX_CONTROL)?;

        // §4.6.5.1: the receiver last, "only after all other setup is
        // accomplished".
        let rx = rctl::EN | rctl::BAM | rctl::SECRC | rctl::BSIZE_2048;
        nic.regs.write(regs::RCTL, rx);
        nic.accepted(regs::RCTL, rx)?;

        // §4.6.5: and only now the mask, so no cause can arrive before there
        // is a ring to answer it with.
        nic.regs.write(regs::IMS, cause::ENABLED | cause::ENABLED_MSIX);

        nic.opened_at = nic.clock.nanos();
        nic.refresh_link();
        Ok(nic)
    }

    /// Raise one enabled cause on purpose (§10.2.4.4).
    ///
    /// **Nothing on a shipping path calls this**: the caller arms it and
    /// [`Self::open`] does not, because a driver that raised a message every
    /// boot would make the kernel's first-message record read the same on a
    /// working card and a dead one. `LSC` is the cause, because acting on it is
    /// re-reading `STATUS`, which the next pass does anyway.
    pub fn provoke_message(&self) {
        self.regs.write(regs::ICS, cause::LSC);
    }

    /// Pass frames sent to the multicast address `group`: the one bit of the
    /// Multicast Table Array its hash names on this part ([`regs::mta_bit_82574`],
    /// [`regs::mta_bit_pch`]) is set, and
    /// every other bit is left as it was.
    pub fn accept_multicast(&self, group: [u8; 6]) {
        let (dword, bit) = match self.part {
            Part::E82574 => regs::mta_bit_82574(group),
            Part::I219 => regs::mta_bit_pch(group),
        };
        let at = regs::MTA + dword * 4;
        self.regs.write(at, self.regs.read(at) | 1 << bit);
    }

    /// A register wrote what it was told, so a window that is not this register
    /// file is refused here instead of looking like a dead network.
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

    /// What the bring-up found, which is the whole of what a boot that raised
    /// no link has to say about why.
    pub fn brought_up(&self) -> BringUp {
        self.brought_up
    }

    pub fn counters(&self) -> Counters {
        self.counters
    }

    /// What the MAC's own statistics registers have counted since the function
    /// was opened: each read clears the register it takes, so every call adds
    /// what it read to what the calls before it did.
    ///
    /// **The part's count and not this driver's.** [`Counters::sent`] is a
    /// descriptor the part wrote back and [`Counters::received`] one it filled;
    /// these are frames at the MAC's own end of the wire — including the ones
    /// its filters dropped and the ones it had no descriptor for — which is
    /// what separates "nothing arrived" from "something arrived and went
    /// nowhere" on a boot with no other record.
    pub fn wire(&mut self) -> Wire {
        let take = |reg| u64::from(self.regs.read(reg));
        let read = Wire {
            sent: take(regs::GPTC),
            received: take(regs::GPRC),
            seen: take(regs::TPR),
            missed: take(regs::MPC),
            crc_errors: take(regs::CRCERRS),
        };
        self.wire = self.wire.plus(read);
        self.wire
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
    /// and left it would find the same one on every later wait. A claim that
    /// refuses to answer at all is not a pass with no messages in it — the
    /// function is no longer this driver's, and the refusal is handed up rather
    /// than counted as quiet.
    pub fn begin_pass(&mut self) -> Result<Pass, I::Refused> {
        self.rx_budget = RX_BUDGET;
        let messages = match self.irq.taken() {
            Ok(count) => count,
            Err(why) if why == I::IDLE => 0,
            Err(why) => return Err(why),
        };

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
        if causes & cause::LSC != 0 || !self.link.is_up() {
            self.refresh_link();
        }
        Ok(Pass { messages, causes, link_changed: self.link != before })
    }

    /// Re-read `STATUS` and take what it says about the link.
    fn refresh_link(&mut self) {
        let status = self.regs.read(regs::STATUS);
        self.link = if status & status::LU != 0 {
            Link::Up {
                speed: Speed::in_status(status),
                full_duplex: status & status::FD != 0,
            }
        } else {
            Link::Down
        };
        if self.link.is_up() && self.link_up_at.is_none() {
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
                    self.counters.received = self.counters.received.saturating_add(1);
                    return Some(Frame { index, at: OFF_RX_BUFS + index * RX_BUF_BYTES, len });
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
                    self.give_back(index);
                }
            }
        }
    }

    /// Give a frame's buffer back to the device, its bytes having been read.
    pub fn rx_done(&mut self, frame: Frame) {
        self.give_back(frame.index);
    }

    /// Republish descriptor `index` and move the tail over every ready
    /// descriptor from where it stands.
    ///
    /// **The tail moves over a run, never to an index.** §7.1.8's tail
    /// "identifies the location beyond the last descriptor hardware can
    /// process", so writing it with the descriptor that came back last would
    /// hand the device every descriptor between — including one whose bytes
    /// their reader still holds — and, for a buffer returned before an older
    /// one, walk `RDT` backwards over the rest of the ring.
    ///
    /// **Ready is read out of the descriptor, so there is no second record of
    /// it to disagree.** [`Self::publish_rx`] zeroes the status word, and a
    /// descriptor still in a caller's hands holds the non-zero word the device
    /// wrote back — `DD` is what [`Self::poll_rx`] took it on. The device
    /// touches neither until the tail passes it.
    fn give_back(&mut self, index: usize) {
        self.publish_rx(index);
        let mut tail = self.rx_tail;
        loop {
            let next = (tail + 1) % RX_RING;
            // Everything from `rx_next` on is the device's or unfilled, and the
            // ring keeps one descriptor in software's hands either way.
            if next == self.rx_next {
                break;
            }
            if self.dma.read(OFF_RX_RING + next * rx_desc::BYTES + 8) != 0 {
                break;
            }
            tail = next;
        }
        if tail == self.rx_tail {
            return;
        }
        self.rx_tail = tail;
        // §7.1.8: the device fetches on the tail write, so the descriptors have
        // to be there before the write is.
        self.dma.publish();
        self.regs.write(regs::RDT, tail as u32);
    }

    /// Take a transmit descriptor and its buffer, or `None` where every one is
    /// in flight or the frame does not fit one.
    ///
    /// Non-blocking by construction. A caller with no slot drops the frame:
    /// spinning on the ring here would park its whole event loop on a device.
    pub fn tx_reserve(&mut self, len: usize) -> Option<TxSlot> {
        // §7.2.10.1: one legacy descriptor carries one buffer, and this
        // driver's is `TX_BUF_BYTES`. A longer frame is refused rather than
        // truncated into one, and counted apart from a full ring because it is
        // a caller that offered more than it was told it could.
        if len > TX_BUF_BYTES {
            self.counters.too_long = self.counters.too_long.saturating_add(1);
            return None;
        }
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
            self.counters.sent = self.counters.sent.saturating_add(1);
        }
    }

    /// Take back every transmit descriptor the part has finished with, so
    /// [`Counters::sent`] says what has left and not only what was handed
    /// over — for a caller about to report it, since the ring is otherwise
    /// reclaimed only when the next frame needs a slot.
    pub fn reclaim(&mut self) {
        self.reclaim_tx();
    }
}

/// What a written-back receive descriptor must satisfy, as bit arithmetic on
/// the one word the device wrote.
///
/// Separate from the volatile reads so the tests can drive it with words no
/// working part would write. Each arm is a way to make this driver act on a
/// number the device chose.
pub(crate) fn parse_rx(word: u64) -> Result<usize, RxRefusal> {
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
