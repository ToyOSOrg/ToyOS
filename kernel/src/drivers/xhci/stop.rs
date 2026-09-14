//! What every reset this kernel performs does to USB before it lets the
//! machine go.
//!
//! **A reset is not a way to end a transfer.** A platform reset stops the
//! controller mid-Bulk-Only-Transport with a device holding half a command, and
//! it leaves VBUS up, so the device never sees a power cycle either: the next
//! host finds a mass-storage device it cannot enumerate, and rebooting the
//! machine does not clear it. What ends a transfer is a **port** reset driven
//! by a controller that is still there to drive it — xHCI 1.2 §5.4.8's PORTSC.PR
//! over §4.19.5's reset signalling — which returns the device to its Default
//! state (USB 2.0 §7.1.7.5) and with it abandons whatever command it was
//! inside.
//!
//! **And a port reset is not free either.** A device need not honour the
//! obligation the class puts on it: cut between its CBW and its CSW, one may
//! enumerate and describe itself perfectly afterwards and answer no SCSI command
//! again until it is physically unplugged, on a laptop whose port power control
//! does not cut VBUS.
//!
//! **So the command is finished before the port is touched.** Bulk-Only
//! Transport §5.1 makes a command three transfers and §6.7.2/§6.7.3 leave a
//! device waiting for whichever has not arrived; the class's own way out is
//! §5.3.4's Reset Recovery, a class request and two CLEAR_FEATURE(HALT)s, all
//! three of them control transfers a wedged kernel cannot issue. What it can do
//! is send the data the CBW promised and read the CSW — [`settle_commands`],
//! bounded, and it cuts when the bound passes, because a machine nobody can
//! turn off is worse still.
//!
//! **Registers and DMA and nothing else, because the panic path calls this.**
//! The controllers are published into [`POINTS`] at bring-up rather than read
//! out of `XHCI`, and the command that is open is published into [`OPEN`] the
//! same way: a panicked CPU may take no lock and allocate nothing, and the CPU
//! it panicked on may be the one holding that lock. Nothing here reads the
//! event ring, which is the one structure that CPU is still the consumer of —
//! the device's own CSW, written into memory this kernel owns, is what says the
//! command completed. The orderly path takes the lock first
//! ([`super::seal_shut`]) so that nothing is open when this runs; the panic path
//! has `halt_all_cpus`'s NMI instead, which stops every other CPU where it stood
//! — possibly mid-command, which is the device this path exists to rescue.

use core::fmt;
use core::sync::atomic::{fence, AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};

use toyos_xhci::bot::{self, Phase};
use toyos_xhci::Portsc;

use crate::log;
use crate::mm::{Dma, Mmio};

use super::{OP_PORT_BASE, OP_USBCMD, OP_USBSTS, PORTSC_PP, PORT_REG_SIZE, USB_TIMEOUT_NS};
use super::{USBCMD_HCRST, USBCMD_RS, USBSTS_CNR, USBSTS_HCH};
use super::{TrbRing, MSC_CSW, MSC_IN_RING, MSC_OUT_RING, MSC_STRIDE, PAGE};
use super::wait::msc::{CSW_LEN, CSW_SIGNATURE};

/// Controllers this path has room for.
///
/// Four, and a fifth is refused by name at bring-up rather than dropped: the
/// target laptop has two and nothing in reach has more, and a controller that
/// silently had no slot is a device this path would leave mid-command.
const SLOTS: usize = 4;

/// How long one controller's connected ports get to finish their reset.
///
/// The driver's own transfer bound, because it is the same question — how long
/// this controller may be given to answer at all — and a port that never clears
/// PR is a controller the account then names.
const PORT_RESET_NS: u64 = USB_TIMEOUT_NS;

/// How long a device gets between the bus reset that ended its transactions and
/// the moment its power goes away.
///
/// **The one margin in this path rather than a derivation.** USB 2.0 §7.1.7.5
/// gives a device 10 ms to recover from a reset (TRSTRCY) and says nothing about
/// how long a mass-storage device's own firmware spends finishing what a
/// SYNCHRONIZE CACHE started; this is ten times TRSTRCY, and it is spent once,
/// at a reset, with nothing else left to do.
const DEVICE_RECOVERY_NS: u64 = 100_000_000;

/// One controller's registers, as numbers a lock-free reader can hold.
struct Point {
    /// The operational register window's address. **Zero until published, and
    /// stored last**, so a reader that sees it sees every field beside it.
    op: AtomicU64,
    op_bytes: AtomicU64,
    /// This function's PCI config window, for the bus-master bit.
    config: AtomicU64,
    /// `bus << 24 | dev << 16 | func << 8 | max_ports`.
    shape: AtomicU32,
}

impl Point {
    // The lint's case is a `const` *read* through, which copies the atomics
    // instead of sharing them. This one is only ever the initialiser of the
    // `static` below, which is the array's own storage.
    #[allow(clippy::declare_interior_mutable_const)]
    const EMPTY: Self = Self {
        op: AtomicU64::new(0),
        op_bytes: AtomicU64::new(0),
        config: AtomicU64::new(0),
        shape: AtomicU32::new(0),
    };
}

static POINTS: [Point; SLOTS] = [Point::EMPTY, Point::EMPTY, Point::EMPTY, Point::EMPTY];

/// What the shutdown's flush pass emptied, for the one summary line.
///
/// **Two atomics and not a return value.** The flush runs above the boot's last
/// word, where a record still reaches the file; this summary is written from
/// `acpi::reboot`, below it. Nothing carries a value across that, and a reset
/// that flushed nothing — every panic — reads the zeros it was born with.
static DISKS: AtomicU32 = AtomicU32::new(0);
static FLUSHED: AtomicU32 = AtomicU32::new(0);
static CACHELESS: AtomicU32 = AtomicU32::new(0);

/// What [`super::flush_disks`] emptied, and how many of those disks had no cache
/// to empty — which is what an `ok` from such a device means and what a line
/// spelling both the same way cannot say.
pub(super) fn flushed(disks: u32, ok: u32, cacheless: u32) {
    DISKS.store(disks, Ordering::Relaxed);
    FLUSHED.store(ok, Ordering::Relaxed);
    CACHELESS.store(cacheless, Ordering::Relaxed);
}

/// Whether a transfer could still have been in flight when the registers below
/// were stopped.
///
/// **The one thing the account cannot read off a register.** A port reset ends a
/// transfer whatever state it was in, so this decides how much the stop had to
/// do — not whether it happened.
#[derive(Clone, Copy)]
pub(super) enum Barrier {
    /// No shutdown path ran: this reset came from a panic, where every other
    /// CPU was stopped by an NMI where it stood, which is not the same claim.
    NotTried = 0,
    /// The shutdown held the controller lock from before the log volume's last
    /// durable byte, so nothing was in flight and nothing could start.
    Taken = 1,
    /// The lock was not free inside its bound. The reset happens anyway: a
    /// machine that cannot be turned off is worse than a transfer cut short.
    Refused = 2,
}

static BARRIER: AtomicU8 = AtomicU8::new(Barrier::NotTried as u8);

/// What [`super::seal_shut`] managed, for the account this module writes.
pub(super) fn barrier(taken: Barrier) {
    BARRIER.store(taken as u8, Ordering::Relaxed);
}

impl Barrier {
    fn said(self) -> &'static str {
        match self {
            Self::NotTried => "usb-quiesce: no barrier was taken, so this reset is not the \
                               shutdown's",
            Self::Taken => "usb-quiesce: the controller lock was held from before the log \
                            volume's last durable byte, so no transfer was in flight",
            Self::Refused => "usb-quiesce: the controller lock was not free inside its bound, \
                              so a transfer may have been in flight",
        }
    }

    fn of(raw: u8) -> Self {
        match raw {
            1 => Self::Taken,
            2 => Self::Refused,
            _ => Self::NotTried,
        }
    }
}

/// The one Bulk-Only command this kernel can have open, as the numbers a
/// lock-free reader can hold.
///
/// **The one thing [`before_reset`] must know and cannot ask.** Its reader takes
/// no lock — the CPU holding `XHCI` may be the wedged one this reset is ending,
/// which is the whole reason [`POINTS`] exists — so the command is published
/// beside the driver rather than read out of it.
///
/// **One and not one per disk.** `with_disk` holds `XHCI` across a whole round
/// trip and the boot scan runs before any AP exists, so no two commands can be
/// open at once; [`OpenCommand::begin`] fails fast on a second rather than
/// overwriting the first, since a command this record did not name is exactly
/// the device a reset would leave mid-phase.
struct Open {
    /// [`Phase::code`], and the release store that publishes every field
    /// beside it: a reader that sees an open phase sees the rest.
    phase: AtomicU8,
    /// The device's block in its controller's DMA pool, as one
    /// [`Dma::addr`]/[`Dma::device_addr`] pair. Both bulk rings, the CSW and
    /// the data buffer live at fixed offsets inside it.
    block: AtomicU64,
    block_device: AtomicU64,
    /// The controller's doorbell window.
    doorbell: AtomicU64,
    doorbell_bytes: AtomicU64,
    /// `slot << 16 | in_dci << 8 | out_dci`.
    endpoints: AtomicU32,
    /// What the CBW told the device the data phase moves, and where; a zero
    /// length is a command that promised none.
    data: AtomicU64,
    data_len: AtomicU32,
    /// Which way it moves, which is the ring it goes on.
    data_in: AtomicBool,
    /// Each bulk ring's next enqueue point as `tail | cycle << 8`, the in ring
    /// in the low half and the out ring in the high one.
    rings: AtomicU32,
}

static OPEN: Open = Open {
    phase: AtomicU8::new(0),
    block: AtomicU64::new(0),
    block_device: AtomicU64::new(0),
    doorbell: AtomicU64::new(0),
    doorbell_bytes: AtomicU64::new(0),
    endpoints: AtomicU32::new(0),
    data: AtomicU64::new(0),
    data_len: AtomicU32::new(0),
    data_in: AtomicBool::new(false),
    rings: AtomicU32::new(0),
};

/// The device and the data phase one Bulk-Only round trip is about to run.
///
/// Every field is the driver's own, and none is read back off the wire: what a
/// finish puts on the bus must be what this kernel promised the device, never
/// what the device then said about it.
pub(in crate::drivers::xhci) struct Device {
    pub block: Dma<'static>,
    pub doorbell: Mmio,
    pub slot: u8,
    pub in_dci: u8,
    pub out_dci: u8,
    /// The data phase's buffer in the device's own address space.
    pub data: u64,
    pub data_len: u32,
    pub data_in: bool,
}

/// Raised for exactly as long as one Bulk-Only command is open on a device.
///
/// **Its lifetime is the command's and not a transfer's.** A device that has
/// taken a CBW and is waiting for its data or its CSW holds no transfer this
/// kernel is inside (BOT §5.1), and that gap — between two of the three phases —
/// is where a port reset leaves the device this path exists to rescue.
#[must_use = "the command counts as open for exactly as long as this lives"]
pub(in crate::drivers::xhci) struct OpenCommand(());

impl OpenCommand {
    /// Publish the device this round trip runs on, with no phase reached yet.
    ///
    /// Called once the CBW is built and before its transfer is queued, so
    /// nothing reaches the controller under a stale device.
    pub(in crate::drivers::xhci) fn begin(on: Device) -> Self {
        assert_eq!(
            OPEN.phase.load(Ordering::Acquire),
            Phase::Closed.code(),
            "xHCI: a second Bulk-Only command was opened while one was still open; \
             `with_disk` holds the controller lock across a whole round trip, so this is a \
             driver bug and the device the record does not name is one a reset would cut"
        );
        OPEN.block.store(on.block.addr(), Ordering::Relaxed);
        OPEN.block_device.store(on.block.device_addr(), Ordering::Relaxed);
        OPEN.doorbell.store(on.doorbell.addr(), Ordering::Relaxed);
        OPEN.doorbell_bytes.store(on.doorbell.size(), Ordering::Relaxed);
        OPEN.endpoints.store(
            (u32::from(on.slot) << 16) | (u32::from(on.in_dci) << 8) | u32::from(on.out_dci),
            Ordering::Relaxed,
        );
        OPEN.data.store(on.data, Ordering::Relaxed);
        OPEN.data_len.store(on.data_len, Ordering::Relaxed);
        OPEN.data_in.store(on.data_in, Ordering::Relaxed);
        Self(())
    }

    /// Publish where the two bulk rings now stand and which phase the round trip
    /// has reached.
    ///
    /// Called after every enqueue and before its doorbell, and at each gap
    /// between two phases: a transfer is never visible to the controller
    /// without the reset being able to see where the ring it went on now is.
    pub(in crate::drivers::xhci) fn at(&self, phase: Phase, in_ring: &TrbRing, out_ring: &TrbRing) {
        OPEN.rings.store(
            u32::from(point(in_ring)) | (u32::from(point(out_ring)) << 16),
            Ordering::Relaxed,
        );
        // Last, and the release: a reader that sees this phase sees the stores
        // above it and the ones `begin` made.
        OPEN.phase.store(phase.code(), Ordering::Release);
    }
}

impl Drop for OpenCommand {
    fn drop(&mut self) {
        OPEN.phase.store(Phase::Closed.code(), Ordering::Release);
    }
}

/// One ring's next enqueue point, packed as `tail | cycle << 8`.
///
/// A byte of tail is the whole of it: [`super::TrbRing::enqueue`] wraps at
/// [`super::RING_SIZE`] minus its link TRB, so the tail never reaches 255.
fn point(ring: &TrbRing) -> u16 {
    debug_assert!(ring.tail < u8::MAX as u16);
    ring.tail | (u16::from(ring.cycle) << 8)
}

/// The open command, as the two views and the numbers a finish needs, or `None`
/// where no device is inside one.
fn inside() -> Option<Inside> {
    let phase = Phase::of(OPEN.phase.load(Ordering::Acquire));
    if !phase.open() {
        return None;
    }
    // SAFETY: the two are one live `Dma<'static>`'s own `addr()` and
    // `device_addr()`, stored by `begin` over a block of the pool the xHCI
    // bring-up leaked for the machine's life; `MSC_STRIDE` is that block's size.
    let block = unsafe {
        Dma::from_addr(
            OPEN.block.load(Ordering::Relaxed),
            OPEN.block_device.load(Ordering::Relaxed),
            MSC_STRIDE,
        )
    };
    let endpoints = OPEN.endpoints.load(Ordering::Relaxed);
    let rings = OPEN.rings.load(Ordering::Relaxed);
    Some(Inside {
        phase,
        block,
        // SAFETY: one live `Mmio`'s own `addr()`/`size()`, as `Point::live`.
        doorbell: unsafe {
            Mmio::from_addr(
                OPEN.doorbell.load(Ordering::Relaxed),
                OPEN.doorbell_bytes.load(Ordering::Relaxed),
            )
        },
        slot: (endpoints >> 16) as u8,
        in_dci: (endpoints >> 8) as u8,
        out_dci: endpoints as u8,
        data: OPEN.data.load(Ordering::Relaxed),
        data_len: OPEN.data_len.load(Ordering::Relaxed),
        data_in: OPEN.data_in.load(Ordering::Relaxed),
        in_ring: ring(block, MSC_IN_RING, rings as u16),
        out_ring: ring(block, MSC_OUT_RING, (rings >> 16) as u16),
    })
}

/// A bulk ring as the driver's own [`super::TrbRing`] left it, rebuilt from the
/// point it published over the page it has always been on.
fn ring(block: Dma<'static>, at: usize, point: u16) -> TrbRing {
    TrbRing {
        buf: block.subview(at, PAGE),
        base_phys: block.device_addr() + at as u64,
        tail: point & 0xFF,
        cycle: point & 0x100 != 0,
    }
}

/// A device inside a command, as this path has to see it.
#[derive(Clone, Copy)]
struct Inside {
    phase: Phase,
    block: Dma<'static>,
    doorbell: Mmio,
    slot: u8,
    in_dci: u8,
    out_dci: u8,
    data: u64,
    data_len: u32,
    data_in: bool,
    in_ring: TrbRing,
    out_ring: TrbRing,
}

impl Inside {
    /// Ring this device's doorbell for one endpoint.
    ///
    /// The driver's own `ring_doorbell` says the same thing through the
    /// controller it borrows; this one has only the window, which is all a
    /// reader that may take no lock can have.
    fn ring_doorbell(self, dci: u8) {
        fence(Ordering::Release);
        self.doorbell.write_u32(u64::from(self.slot) * 4, u32::from(dci));
    }

    /// The CSW the device writes, as the region this path polls and reads.
    fn csw(self) -> Dma<'static> {
        self.block.subview(MSC_CSW, CSW_LEN as usize)
    }
}

/// How long each of this path's two waits on a device gets.
///
/// The driver's own transfer bound, because both ask the same question — is
/// this device still answering — and past it neither a longer wait nor a
/// shorter one changes what the reset then has to do.
const BOT_NS: u64 = USB_TIMEOUT_NS;

/// Finish the Bulk-Only command this kernel has open, and say what that took.
///
/// **The reset's own transfers, on rings the driver built.** Everything here is
/// a TRB on a ring this kernel owns, a doorbell write and a poll of the DMA the
/// device writes its CSW into: no lock, no allocation, and no event ring, which
/// is the one structure the wedged CPU is still the consumer of.
///
/// What the device is owed is [`bot::owed`]'s to decide; a transfer already
/// queued is the controller's to finish and queueing it again would move the
/// same bytes twice — for an out data phase, a second write of the block.
fn finish(inside: Inside, said: &mut dyn fmt::Write) {
    let Some(owed) = bot::owed(inside.phase, inside.data_len) else { return };
    let (mut in_ring, mut out_ring) = (inside.in_ring, inside.out_ring);
    if owed.ring_data {
        let (dci, ring) = match inside.data_in {
            true => (inside.in_dci, &mut in_ring),
            false => (inside.out_dci, &mut out_ring),
        };
        if owed.data {
            ring.enqueue(super::normal_trb(inside.data, inside.data_len));
        }
        inside.ring_doorbell(dci);
    }
    if owed.status {
        // Zeroed here because this is the transfer that fills it: a CSW the
        // driver already queued was zeroed by the driver, and zeroing it again
        // would discard the answer the poll below is waiting for.
        super::zero_dma(inside.block, MSC_CSW, CSW_LEN as usize);
        in_ring.enqueue(super::normal_trb(
            inside.block.device_addr() + MSC_CSW as u64,
            CSW_LEN,
        ));
        inside.ring_doorbell(inside.in_dci);
    }
    let csw = inside.csw();
    let answered = crate::clock::settles(BOT_NS, || {
        u32::from_le(csw.read::<u32>(0)) == CSW_SIGNATURE
    });
    let _ = writeln!(
        said,
        "usb-quiesce: a Bulk-Only command was open in its {} phase on slot {}, so this reset \
         sent the {} B it owed and asked for the status",
        inside.phase,
        inside.slot,
        if owed.data { inside.data_len } else { 0 },
    );
    match answered {
        // The device is back where BOT §5.1 leaves it between commands, which
        // is the state a port reset is defined over.
        true => {
            let _ = writeln!(
                said,
                "usb-quiesce: the device answered with a CSW of status {:#04x}, {} B unmoved, so \
                 this reset cuts no command",
                csw.read::<u8>(12),
                u32::from_le(csw.read::<u32>(8)),
            );
        }
        false => {
            let _ = writeln!(
                said,
                "usb-quiesce: the device sent no CSW inside {} ms, so this reset cuts the command \
                 — a device cut between its CBW and its CSW may need a physical replug before its \
                 next host can enumerate it",
                BOT_NS / 1_000_000,
            );
        }
    }
}

/// Settle the **protocol** and not only its transfers, and say which of the
/// three this reset was.
///
/// **A port reset in a command's data phase is not free, whatever the class
/// says.** USB Mass Storage Bulk-Only Transport §5.3.4 makes reset recovery the
/// device's obligation and the port reset below clears strictly more than it
/// asks for; a device that does not honour it is one no software on a laptop
/// with no VBUS control can clear.
///
/// So the driver is given its own bound to close the command it has open, and
/// where that does not happen this path finishes the command itself. Both are
/// bounded and the reset follows either way: a machine nobody can turn off is
/// worse than a device somebody has to replug, and the account is what says
/// which of the two this reset was.
fn settle_commands(said: &mut dyn fmt::Write) {
    if inside().is_none() {
        let _ = writeln!(
            said,
            "usb-quiesce: no Bulk-Only command was open, so this reset cuts none"
        );
        return;
    }
    crate::clock::settles(BOT_NS, || inside().is_none());
    // Read again rather than reusing the first: a command that walked a phase
    // inside the wait owes what it owes now, not what it owed then.
    let Some(open) = inside() else {
        let _ = writeln!(
            said,
            "usb-quiesce: the Bulk-Only command that was open was closed by its own driver \
             inside {} ms, so this reset cuts none",
            BOT_NS / 1_000_000,
        );
        return;
    };
    finish(open, said);
}

/// Entered once for the machine's life.
///
/// **A panic inside this path would otherwise reach it a second time**: the
/// panic path ends in `acpi::reboot`, which is the very site that calls this.
static STOPPING: AtomicBool = AtomicBool::new(false);

/// Record a controller this kernel drives, so a reset can stop it without the
/// lock that guards the driver.
///
/// Called from the bring-up for every controller that came up, and for none
/// that was refused: a refused controller had no transfer from this kernel on
/// it, and the ones refused after their own halt and HCRST are already stopped.
pub(super) fn publish(op: Mmio, config: Mmio, bus: u8, dev: u8, func: u8, max_ports: u8) {
    let shape = (u32::from(bus) << 24)
        | (u32::from(dev) << 16)
        | (u32::from(func) << 8)
        | u32::from(max_ports);
    for point in POINTS.iter() {
        if point.op.load(Ordering::Relaxed) != 0 {
            continue;
        }
        point.op_bytes.store(op.size(), Ordering::Relaxed);
        point.config.store(config.addr(), Ordering::Relaxed);
        point.shape.store(shape, Ordering::Relaxed);
        // Last, and the release: a reader that sees this address sees the three
        // stores above it.
        point.op.store(op.addr(), Ordering::Release);
        return;
    }
    log!(
        "xHCI: {bus:02x}:{dev:02x}.{func} is past the {SLOTS} controllers a reset can stop, so a \
         reset would leave its devices mid-command — refused"
    );
}

/// A published controller, as the two windows a stop drives.
#[derive(Clone, Copy)]
struct Live {
    op: Mmio,
    config: Mmio,
    bus: u8,
    dev: u8,
    func: u8,
    max_ports: u8,
}

impl Point {
    fn live(&self) -> Option<Live> {
        let at = self.op.load(Ordering::Acquire);
        if at == 0 {
            return None;
        }
        let shape = self.shape.load(Ordering::Relaxed);
        Some(Live {
            // SAFETY: `at` and the size beside it are one live `Mmio`'s own
            // `addr()`/`size()`, stored by `publish` from the window the
            // bring-up mapped and which lives for the machine's life.
            op: unsafe { Mmio::from_addr(at, self.op_bytes.load(Ordering::Relaxed)) },
            // SAFETY: the same, for the config window `PciDevice` carries.
            config: unsafe {
                Mmio::from_addr(self.config.load(Ordering::Relaxed), crate::drivers::pci::CONFIG_BYTES)
            },
            bus: (shape >> 24) as u8,
            dev: (shape >> 16) as u8,
            func: (shape >> 8) as u8,
            max_ports: shape as u8,
        })
    }
}

impl Live {
    fn portsc_at(self, port: u8) -> u64 {
        OP_PORT_BASE + u64::from(port) * PORT_REG_SIZE
    }

    /// Reset every connected port, halt the controller, reset it, take the
    /// ports' power away and stop its bus mastering.
    ///
    /// Every step says what it *did* and not what it asked for: each register
    /// is read back, because a controller that ignored a write and one that took
    /// it are the same instruction and different machines.
    fn stop(self, tally: &mut Stopped, said: &mut dyn fmt::Write) {
        tally.controllers += 1;
        let (bus, dev, func) = (self.bus, self.dev, self.func);
        let at = format_args!("{bus:02x}:{dev:02x}.{func}");

        // **First, and while the controller is still running.** This is the one
        // act an attached device sees: PR drives USB reset signalling on the
        // port (xHCI 1.2 §4.19.5), and a device that takes it is back in its
        // Default state with no command outstanding (USB 2.0 §7.1.7.5). Every
        // connected port is set going before any is waited on, because they
        // reset in parallel and waiting per port would add their bounds up.
        let mut connected: u32 = 0;
        for port in 0..self.max_ports {
            let portsc = Portsc::from_raw(self.op.read_u32(self.portsc_at(port)));
            if !portsc.connected() {
                continue;
            }
            connected += 1;
            self.op.write_u32(self.portsc_at(port), portsc.neutral().resetting().raw());
        }
        let still_resetting = || {
            (0..self.max_ports)
                .filter(|port| Portsc::from_raw(self.op.read_u32(self.portsc_at(*port))).in_reset())
                .count() as u32
        };
        if connected > 0 {
            crate::clock::settles(PORT_RESET_NS, || still_resetting() == 0);
        }
        // What the ports did, not what they were asked: a port still holding PR
        // after the bound is a controller that never drove the reset.
        let finished = connected.saturating_sub(still_resetting());
        tally.connected += connected;
        tally.reset_ports += finished;
        let _ = writeln!(said, "usb-quiesce: xHCI {at} {finished}/{connected} connected port(s) reset");

        // The device's own recovery, and whatever its firmware is still doing
        // with what the last SYNCHRONIZE CACHE handed it, before anything else
        // moves under it.
        crate::clock::settles(DEVICE_RECOVERY_NS, || false);

        // Run/Stop cleared: a halted controller is reading no ring and driving
        // no endpoint, which is what makes the HCRST below a defined act
        // (xHCI 1.2 §5.4.1).
        let cmd = self.op.read_u32(OP_USBCMD);
        self.op.write_u32(OP_USBCMD, cmd & !USBCMD_RS);
        let halted = crate::clock::settles(USB_TIMEOUT_NS, || {
            self.op.read_u32(OP_USBSTS) & USBSTS_HCH != 0
        });
        tally.halted += u32::from(halted);
        let _ = writeln!(
            said,
            "usb-quiesce: xHCI {at} halted={halted} USBSTS={:#010x}",
            self.op.read_u32(OP_USBSTS)
        );

        // The controller back to its own defaults, so the next kernel — or this
        // machine's firmware — finds no ring it did not build (§4.22.1).
        self.op.write_u32(OP_USBCMD, USBCMD_HCRST);
        let cleared = crate::clock::settles(USB_TIMEOUT_NS, || {
            self.op.read_u32(OP_USBCMD) & USBCMD_HCRST == 0
        });
        let ready = crate::clock::settles(USB_TIMEOUT_NS, || {
            self.op.read_u32(OP_USBSTS) & USBSTS_CNR == 0
        });
        tally.reset += u32::from(cleared && ready);
        let _ = writeln!(said, "usb-quiesce: xHCI {at} reset={cleared} ready={ready}");

        // Port power away where this controller has switches to take it away
        // with, and after HCRST because HCRST puts PORTSC back to its default —
        // which on a controller with Port Power Control is unpowered already. A
        // controller without it ignores the write, and the read-back count is
        // what says which kind this one is.
        let mut down: u32 = 0;
        for port in 0..self.max_ports {
            let off = self.portsc_at(port);
            let portsc = Portsc::from_raw(self.op.read_u32(off));
            self.op.write_u32(off, portsc.neutral().unpowered().raw());
            if self.op.read_u32(off) & PORTSC_PP == 0 {
                down += 1;
            }
        }
        tally.ports += u32::from(self.max_ports);
        tally.unpowered += down;
        let _ = writeln!(said, "usb-quiesce: xHCI {at} {down}/{} port(s) unpowered", self.max_ports);

        // Last, because everything above is an MMIO write this path still
        // needed: after it the function can issue no DMA at all, so nothing
        // stale reaches memory the next kernel owns.
        crate::drivers::pci::stop_bus_mastering(self.config);
        let _ = writeln!(said, "usb-quiesce: xHCI {at} bus mastering off");
    }
}

/// Stop every controller this kernel drives, and say what that did.
///
/// The one entry, taken by the orderly shutdown and by a panic alike. Takes no
/// lock, allocates nothing, and every wait in it is bounded against the TSC.
fn stop_all(said: &mut dyn fmt::Write) {
    let mut tally = Stopped::NOTHING;
    tally.disks = DISKS.load(Ordering::Relaxed);
    tally.flushed = FLUSHED.load(Ordering::Relaxed);
    tally.cacheless = CACHELESS.load(Ordering::Relaxed);
    let _ = writeln!(said, "{}", Barrier::of(BARRIER.load(Ordering::Relaxed)).said());
    // Before the first register of the first controller: the record is
    // machine-wide, and a command on one controller is not made safe by
    // stopping another first.
    settle_commands(said);
    for point in POINTS.iter() {
        if let Some(live) = point.live() {
            live.stop(&mut tally, said);
        }
    }
    let _ = writeln!(said, "{tally}");
}

/// Hand every USB device back, then let the caller end the machine.
///
/// **Called from `acpi::reboot` and `acpi::shutdown` and nowhere else**, which
/// is what makes "no reset this kernel performs leaves a USB device
/// mid-command" a property of the reset rather than of whoever asked for one.
pub fn before_reset() {
    if STOPPING.swap(true, Ordering::AcqRel) {
        return;
    }
    // The account goes into the black box, under whatever this boot already
    // sealed there. It reaches no file: the log volume's own device is one of
    // the ones being taken down, and on the orderly path the volume's last byte
    // is already durable. The next boot's loader is its only reader.
    if !crate::blackbox::append(|out| stop_all(out)) {
        stop_all(&mut Unheard);
    }
}

/// Where the account goes on a boot with no page to put it on. The stop happens
/// either way: it is what the machine needs, and the account is what a reader of
/// the next boot needs.
struct Unheard;

impl fmt::Write for Unheard {
    fn write_str(&mut self, _: &str) -> fmt::Result {
        Ok(())
    }
}

/// What the reset did to this machine's USB.
///
/// **A count per step and not a verdict**: a controller that halted and one that
/// was merely asked to are the difference this whole path exists for, and the
/// next boot's loader is the only reader.
struct Stopped {
    disks: u32,
    flushed: u32,
    cacheless: u32,
    controllers: u32,
    connected: u32,
    reset_ports: u32,
    halted: u32,
    reset: u32,
    ports: u32,
    unpowered: u32,
}

impl Stopped {
    const NOTHING: Self = Self {
        disks: 0,
        flushed: 0,
        cacheless: 0,
        controllers: 0,
        connected: 0,
        reset_ports: 0,
        halted: 0,
        reset: 0,
        ports: 0,
        unpowered: 0,
    };
}

impl fmt::Display for Stopped {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "usb-quiesce: {}/{} disk cache(s) flushed, {} with no cache to flush, \
             {}/{} connected port(s) reset, {}/{} controller(s) halted, {} reset, \
             {}/{} port(s) unpowered",
            self.flushed,
            self.disks,
            self.cacheless,
            self.reset_ports,
            self.connected,
            self.halted,
            self.controllers,
            self.reset,
            self.unpowered,
            self.ports
        )
    }
}
