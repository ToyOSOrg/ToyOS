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
//! inside. Bulk-Only Transport §5.3.4's own reset recovery is a class request
//! and two CLEAR_FEATURE(HALT)s, all of them transfers on a live device: a
//! panicked kernel can issue none of them, and a port reset clears strictly
//! more than they do.
//!
//! **Registers and nothing else, because the panic path calls this.** The
//! controllers are published into [`POINTS`] at bring-up rather than read out of
//! `XHCI`: a panicked CPU may take no lock and allocate nothing, and the CPU it
//! panicked on may be the one holding that lock. The orderly path takes the
//! lock first ([`super::seal_shut`]) so that no transfer is in flight when this
//! runs; the panic path has `halt_all_cpus`'s NMI instead, which stops every
//! other CPU where it stood — possibly mid-transfer, which is the device this
//! path exists to rescue.

use core::fmt;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};

use toyos_xhci::Portsc;

use crate::log;
use crate::mm::Mmio;

use super::{OP_PORT_BASE, OP_USBCMD, OP_USBSTS, PORTSC_PP, PORT_REG_SIZE, USB_TIMEOUT_NS};
use super::{USBCMD_HCRST, USBCMD_RS, USBSTS_CNR, USBSTS_HCH};

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
