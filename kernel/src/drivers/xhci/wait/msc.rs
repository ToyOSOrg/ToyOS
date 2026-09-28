//! USB Mass Storage, Bulk-Only Transport with SCSI (interface class 0x08,
//! subclass 0x06, protocol 0x50) only — no UAS, no CBI/CB, one logical unit,
//! no removable-media handling, no MODE SENSE. One command in flight per
//! controller: `with_disk` holds the controller lock for the whole of it.
//! Everything here comes off the wire and is checked, never trusted; refusal
//! is by name, never a panic.

use crate::mm::Dma;

use crate::block::{BlockError, BlockResult};
use crate::log;
use crate::scheduler::Operation;
use crate::time::{Budget, Deadline, Duration};
use super::{Control, Quiet, Restart};
use super::super::{log_unrecoverable, Completion, Disk, StorageGeometry, Trb};
use super::super::{look_for, ports_wanted, with_disk_by, Whereabouts};
use super::super::{TrbRing, XhciController, PAGE, TRB_ADDRESS_DEVICE, TRB_CONFIGURE_EP, TRB_RESET_DEVICE};
use super::super::{stop, CC_SUCCESS, TRB_NORMAL, OFF_INPUT_CTX};
use super::super::{AFTER_BREAK, CC_CONTEXT_STATE_ERROR, EP0_DCI};
use super::super::{MSC_IN_RING, MSC_OUT_RING, MSC_CBW, MSC_CSW, MSC_SCRATCH, MSC_SCRATCH_LEN};
use super::super::MSC_INPUT_CTX;
use super::super::{MSC_DATA, MSC_DATA_LEN, MSC_MAX_BLOCKS, MSC_STRIDE};
use super::super::device::Endpoint;
use toyos_xhci::bot::{self, Bot, Phase, RoundTrip, CBW_LEN, CSW_LEN};
use toyos_xhci::call::{AfterBreak, NotIssued};
use toyos_xhci::configure::{self, BulkEndpoint};
use toyos_xhci::flush::Debt;
use toyos_xhci::identity::{self, Identity, Serial, UsbId};
use toyos_xhci::ladder::{self, AfterReset, Left, PortStep, Run, Rung};
use toyos_xhci::port;
use toyos_xhci::reset_recovery::{self, Answered, GaveUp, Look, Pipe, Quiescing, SlotGoes, Step};
use toyos_xhci::scsi::{self, BringUp, Cdb, Fail, Flushed, Geometry, Heard, Moved, Printable, Reply};
use toyos_xhci::scsi::{Refusal, Sense, Transfer, HOST_BLOCK};

/// A region, not an address: the CBW's length is the region's own size, so
/// no command can name a length its destination lacks.
type DataPhase = Option<Dma<'static>>;

/// Why a round trip broke, with this driver's reason for a silence.
type Broke = bot::Broke<Quiet>;

/// Wall-clock budget on bring-up's ready attempts: bounds when [`bring_up`]
/// stops *starting* attempts, not the one already running.
const READY_BUDGET: Budget = Budget::of(
    Duration::from_millis(500),
    "the device is reported as not becoming ready and the boot goes on without it",
);

/// The most breaks a device's transport gets in a row before the device is
/// offline: one per rung of `toyos_xhci::ladder`.
pub(in crate::drivers::xhci) const MAX_TRANSPORT_BREAKS: u8 = ladder::MOST_BREAKS;

/// What the configuration descriptor said about a mass-storage interface;
/// both endpoints, always, each valid because `Endpoint` only comes from
/// its own constructor.
#[derive(Clone, Copy)]
pub struct MscInterface {
    pub iface_num: u8,
    pub in_ep: Endpoint,
    pub out_ep: Endpoint,
}

/// One bound disk; `Copy` so a command can borrow the controller and the
/// device's own rings at once without the controller borrowing itself.
#[derive(Clone, Copy)]
pub struct MscDevice {
    slot_id: u8,
    /// The interface and its two bulk endpoints as the descriptor gave them,
    /// kept because a Reset Recovery re-creates both endpoints and has to
    /// describe them to the controller exactly as the bind did.
    pair: MscInterface,
    /// The root-hub port the device is on, known from the first command of its
    /// bind — before the port itself is told which slot it holds.
    port_idx: u8,
    /// What the enumeration addressed and configured the device with, which
    /// the port rung addresses and configures it with again: the speed its port
    /// trained at, the control endpoint's packet size, and the configuration
    /// value.
    enumerated: Enumerated,
    /// Byte offset of this device's block in its controller's DMA pool.
    block: usize,
    /// Byte offset of the device context block; its endpoint states decide
    /// which recovery command is legal.
    dev_block: usize,
    ep0_ring: TrbRing,
    in_ring: TrbRing,
    out_ring: TrbRing,
    tag: u32,
    /// Zero until bring-up reads the disk's size.
    geometry: Geometry,
    run: Run,
    /// Set once the device was taken offline; the device is not spoken to again.
    failed: bool,
    /// Where [`XhciController::take_offline`] said the slot goes, until
    /// somebody has acted on it; see [`Self::take_slot_owed`].
    slot_goes: Option<SlotGoes>,
    /// Set once the device refuses SYNCHRONIZE CACHE; logged once, not per
    /// flush — a log line would itself be pending content the next flush drains.
    no_write_cache: bool,
    /// The flush this instance owes for its own writes, and how many devices
    /// serving this disk left owing one ([`Self::owes_a_flush`]).
    debt: Debt,
    /// What the device says it is, which a device that binds after this one
    /// left must match to take its number.
    identity: Identity,
    /// When this driver last reset the device's port.
    reset_at: Option<u64>,
    /// The device's port was found empty inside a call; see
    /// [`XhciController::hold_for_return`].
    left: bool,
}

impl MscDevice {
    /// Whether the driver will still speak to this device — distinct from
    /// `blocks > 0`, which survives a failure.
    #[cfg(feature = "boot-actuators")]
    pub fn online(&self) -> bool {
        !self.failed
    }

    /// Whether this device has answered SYNCHRONIZE CACHE with INVALID COMMAND
    /// OPERATION CODE, which is what makes a flush's `Ok(())` mean "there was
    /// nothing to make durable" rather than "a cache was emptied".
    pub(in crate::drivers::xhci) fn refused_flush(&self) -> bool {
        self.no_write_cache
    }

    /// Which slot this disk is on.
    pub fn slot_id(&self) -> u8 {
        self.slot_id
    }

    /// The port whose slot is owed back to the controller, once: the disk was
    /// taken offline with its endpoints Stopped, and its slot is still enabled.
    pub(in crate::drivers::xhci) fn take_slot_owed(&mut self) -> Option<u8> {
        if self.slot_goes != Some(SlotGoes::Back) {
            return None;
        }
        self.slot_goes = None;
        Some(self.port_idx)
    }

    /// Until when the disk is held for its device to come back, seen gone at
    /// `now`: only after a reset of this driver's (`toyos_xhci::identity`).
    pub(in crate::drivers::xhci) fn returns_by(&self, now: u64) -> Option<u64> {
        identity::returns_by(self.reset_at, now)
    }

    pub(in crate::drivers::xhci) fn identity(&self) -> &Identity {
        &self.identity
    }

    /// Whether a device that left now could take writes reported complete with
    /// it: held in a volatile cache no flush has emptied since
    /// (`toyos_xhci::flush`).
    pub(in crate::drivers::xhci) fn owes_a_flush(&self) -> bool {
        self.debt.owed(self.no_write_cache)
    }

    /// The disk's loss count this device hands the one that takes the disk
    /// back, were it to leave now.
    pub(in crate::drivers::xhci) fn losses_left(&self) -> u64 {
        self.debt.left(self.no_write_cache)
    }

    /// The device reported a transfer complete, whole or in part; `write` is
    /// whether it was one.
    fn wrote(&mut self, write: bool) {
        if write {
            self.debt.wrote();
            #[cfg(feature = "boot-actuators")]
            transport_break::wrote();
        }
    }

    pub(in crate::drivers::xhci) fn port_idx(&self) -> u8 {
        self.port_idx
    }

    /// Where the disk is and what it says it is: its root-hub port, the speed
    /// that port trained at, and its device descriptor's ids.
    pub fn inventory(&self) -> (u8, toyos_abi::inventory::UsbSpeed, toyos_xhci::identity::UsbId) {
        (self.port_idx, self.enumerated.usb_speed, self.identity.usb)
    }

    pub fn geometry(&self) -> StorageGeometry {
        StorageGeometry {
            logical_block_bytes: self.geometry.sector_bytes(),
            blocks: self.geometry.blocks(),
        }
    }

    fn next_tag(&mut self) -> u32 {
        self.tag = self.tag.wrapping_add(1);
        self.tag
    }

    fn in_dci(&self) -> u8 {
        self.pair.in_ep.dci()
    }

    fn out_dci(&self) -> u8 {
        self.pair.out_ep.dci()
    }

    /// One bulk endpoint as the controller names it.
    fn dci(&self, pipe: Pipe) -> u8 {
        match pipe {
            Pipe::In => self.in_dci(),
            Pipe::Out => self.out_dci(),
        }
    }

    /// The same endpoint as the *device* names it, for CLEAR_FEATURE.
    fn ep_addr(&self, pipe: Pipe) -> u8 {
        match pipe {
            Pipe::In => self.pair.in_ep.addr,
            Pipe::Out => self.pair.out_ep.addr,
        }
    }

    /// One bulk endpoint's ring and where in the pool it lives.
    fn ring_mut(&mut self, pipe: Pipe) -> (&mut TrbRing, usize) {
        match pipe {
            Pipe::In => (&mut self.in_ring, self.block + MSC_IN_RING),
            Pipe::Out => (&mut self.out_ring, self.block + MSC_OUT_RING),
        }
    }
}

/// Who a round trip is for, which is whose staging it takes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Asks {
    /// A caller's command, or the bind's.
    Command,
    /// The TEST UNIT READY this rung of the ladder ends on.
    Verification(Rung),
}

/// What [`MscDevice::enumerated`] holds.
#[derive(Clone, Copy)]
pub(in crate::drivers::xhci) struct Enumerated {
    pub speed: u8,
    /// `speed` decoded once, at bind, where a psiv with no default Protocol
    /// Speed ID already refused the port.
    pub usb_speed: toyos_abi::inventory::UsbSpeed,
    pub ep0_packet: u16,
    pub configuration: u8,
}

/// A break as its line says it.
struct Told<'a>(&'a Broke);

impl core::fmt::Display for Told<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            Broke::Code { phase, code, .. } => {
                write!(f, "{phase} phase completion {}", Completion(*code))
            }
            Broke::Silence { phase, why } => why.about(phase.named(), "phase", f),
            Broke::Gone { phase } => Quiet::Gone.about(phase.named(), "phase", f),
            Broke::Short { phase, moved, wanted } => {
                write!(f, "{phase} phase moved {moved} of {wanted} B")
            }
            Broke::Stall { phase } => {
                write!(f, "the {phase} phase stalled and the endpoint reset did not clear it")
            }
            Broke::PhaseError => f.write_str("the device reported a phase error"),
            Broke::Reserved { status } => {
                write!(f, "the CSW carries status {status:#04x}, which the class reserves")
            }
            Broke::Csw { what, got, want, status, residue } => write!(
                f,
                "CSW {what} {got:#x}, not {want:#x} (status {status}, {residue} B unmoved)"
            ),
            Broke::Residue { unmoved, of } => {
                write!(f, "CSW claims {unmoved} B unmoved of {of}")
            }
        }
    }
}

/// How one rung of the ladder ended.
enum Climbed {
    /// The device answered the rung's TEST UNIT READY under that command's own
    /// tag.
    InStep,
    /// Everything before the TEST UNIT READY was answered, and it broke this
    /// way: a break like the one recovered from, and counted as one.
    OutOfStep(Broke),
    /// A step before the TEST UNIT READY was not answered, and said so; the
    /// device was asked nothing after it.
    Failed,
    /// The port reads empty: the device is no longer on the bus, and its
    /// port's teardown owns what it held.
    Gone,
}

/// Abandon one bulk transfer without waiting, once per boot, on the first
/// WRITE(10): only the wait is skipped, so recovery runs against a real
/// endpoint state — staged since nothing on the host side leaves one in flight.
#[cfg(feature = "boot-actuators")]
mod transport_break {
    use core::sync::atomic::{AtomicBool, Ordering};

    static UNSPENT: AtomicBool = AtomicBool::new(true);
    static ARMED: AtomicBool = AtomicBool::new(false);
    /// A write was reported complete, and then a SYNCHRONIZE CACHE succeeded
    /// with none since: kept here and not read off the driver's own debt, so
    /// `usb-transport-break-flushed` stages what it says whatever that debt
    /// holds.
    static WROTE: AtomicBool = AtomicBool::new(false);
    static FLUSHED: AtomicBool = AtomicBool::new(false);

    /// What the break `usb-transport-break-flushed` stages says before it.
    pub const AFTER_A_FLUSH: &str = "breaks next (usb-transport-break-flushed): a write was \
        reported complete and a SYNCHRONIZE CACHE succeeded after it, with no write since";

    /// A write was reported complete.
    pub fn wrote() {
        WROTE.store(true, Ordering::Relaxed);
        FLUSHED.store(false, Ordering::Relaxed);
    }

    /// A SYNCHRONIZE CACHE succeeded.
    pub fn flushed() {
        FLUSHED.store(WROTE.load(Ordering::Relaxed), Ordering::Relaxed);
    }

    /// Called where the driver is about to run a WRITE(10) data phase; `owed`
    /// is whether its device owes a flush (`MscDevice::owes_a_flush`), which
    /// `usb-transport-break-owed` waits for.
    pub fn arm(owed: bool) {
        let after_a_flush =
            crate::actuator::usb_transport_break_flushed() && FLUSHED.load(Ordering::Relaxed);
        let wanted = crate::actuator::usb_transport_break()
            || (crate::actuator::usb_transport_break_owed() && owed)
            || after_a_flush;
        if !wanted {
            return;
        }
        let armed = UNSPENT.swap(false, Ordering::Relaxed);
        if armed && after_a_flush {
            crate::log!("usb-storage: the WRITE(10) going out {AFTER_A_FLUSH}");
        }
        ARMED.store(armed, Ordering::Relaxed);
    }

    /// Called after the doorbell, where the wait would otherwise begin.
    pub fn take() -> bool {
        ARMED.swap(false, Ordering::Relaxed)
    }
}

/// Stop every CPU inside one WRITE(10), at whichever of its three phases was
/// staged.
///
/// **The state a boot that hangs during stick I/O leaves the device in**, and
/// the one thing no ordinary boot reaches: a machine wedged here is ended by
/// the boot deadline alone, and what the reset then does to a device holding
/// half a command is what `stop::settle_commands` exists to decide.
///
/// **Which phase is the whole question, so the caller names one.** The device
/// sees three different things — a CBW with no data coming, a data phase queued
/// and not rung for, and data it has taken with nothing asking for its CSW —
/// and only the machine can say which of them it does not come back from.
///
/// Staged rather than waited for, because a shutdown reaches its sync with
/// nothing dirty on most boots: the write this is taken inside is one
/// `usb_gate::wedge_inside_a_write` issues for it.
#[cfg(feature = "boot-actuators")]
pub(in crate::drivers::xhci) mod mid_write {
    use core::sync::atomic::{AtomicU8, Ordering};

    use toyos_xhci::bot::Phase;

    /// [`Phase::code`] of the phase to stop at, or `Phase::Closed`'s — which no
    /// call site passes — for a boot that staged none.
    static AT: AtomicU8 = AtomicU8::new(0);

    /// Called immediately before the write this wedge is taken inside.
    pub fn arm(at: Phase) {
        AT.store(at.code(), Ordering::Relaxed);
    }

    /// Called at each of the three phases, each passing its own.
    ///
    /// Compare-and-take, not a test and a clear: every command walks all three
    /// call sites, so a phase that takes the staging away from the phase it was
    /// staged for is a boot that wedges nowhere.
    pub fn wedge_if_staged(here: Phase) {
        let taken = AT.compare_exchange(
            here.code(),
            Phase::Closed.code(),
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
        if taken.is_ok() {
            crate::deadline::stage_a_wedge()
        }
    }
}

/// Have every transfer of the operation sent again on a device that came back
/// answer nothing, once, each waited for to the end of what the call lets it
/// spend: a returning device that stops answering, which spends the whole of
/// what the held call has left — staged because nothing on the host side stops
/// a device answering.
#[cfg(feature = "boot-actuators")]
pub(in crate::drivers::xhci) mod return_silent {
    use core::sync::atomic::{AtomicBool, Ordering};

    static UNSPENT: AtomicBool = AtomicBool::new(true);
    static ACTIVE: AtomicBool = AtomicBool::new(false);

    /// What the staged operation's disk says before it goes out again.
    pub const SILENT: &str =
        "answers nothing on the operation sent again on it (usb-return-silent)";

    /// `true` means the operation disk `index` is about to send again is the
    /// staged one, and must call [`end`] once it has.
    pub fn begin(index: usize) -> bool {
        if !crate::actuator::usb_return_silent() || !UNSPENT.swap(false, Ordering::Relaxed) {
            return false;
        }
        crate::log!("usb-storage: disk {index} {SILENT}");
        ACTIVE.store(true, Ordering::Relaxed);
        true
    }

    pub fn end() {
        ACTIVE.store(false, Ordering::Relaxed);
    }

    /// Only the staged operation's own transfers can see this set: it holds
    /// the controller lock throughout.
    pub fn active() -> bool {
        ACTIVE.load(Ordering::Relaxed)
    }
}

/// Stage the boot's first bind to spend the scan's whole silence bound and then
/// refuse, once: T14 run 103's stick, whose first command went unanswered, whose
/// recovery ladder then ran on bounds of its own, and whose refusal at the end
/// of all that submitted a Disable Slot into a scan that had already stopped
/// listening. Staged because no QEMU device stops answering its first command.
///
/// The spending and the refusal are staged and the ladder is not: what the
/// defect needs is a submit later than the scan's bound, and which rungs ran
/// decides nothing about that.
///
/// **The held answer is the second half and not decoration.** QEMU posts a
/// Command Completion Event inside the vCPU's write to the doorbell, so the very
/// next read of the event ring already has it and the scan's silence bound is
/// re-armed before it can be spent — on a machine whose controller takes
/// microseconds to answer it is not. [`hold_answers`] is that latency, and
/// without it no QEMU boot reaches the state this stages.
#[cfg(feature = "boot-actuators")]
pub(in crate::drivers::xhci) mod bind_spends_the_scan {
    use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    /// Past the scan's bound, which is `USB_TIMEOUT_NS`: the refusal has to
    /// land after the scan has stopped re-arming it, and a bind that spent
    /// exactly the bound would race it.
    pub const SPEND: u64 = super::super::super::USB_TIMEOUT_NS + 200_000_000;

    /// How long the controller's answer to what the refusal submits is held
    /// back. It has only to outlast the one loop iteration that follows the
    /// submit; the width above that is so a scan that waits can be seen to.
    const HOLD: u64 = 50_000_000;

    pub const WHY: &str = "answers nothing for the boot scan's whole bound \
        (usb-bind-spends-the-scan) and is then refused";

    static UNSPENT: AtomicBool = AtomicBool::new(true);
    static HELD_UNTIL: AtomicU64 = AtomicU64::new(0);

    /// Whether this bind is the staged one.
    pub fn take() -> bool {
        crate::actuator::usb_bind_spends_the_scan() && UNSPENT.swap(false, Ordering::Relaxed)
    }

    /// Hold the controller's answers back, from the staged bind's way out — so
    /// the window covers what its refusal submits and nothing before it.
    pub fn hold_answers() {
        HELD_UNTIL.store(crate::clock::nanos_since_boot() + HOLD, Ordering::Relaxed);
    }

    /// Whether the event ring may be read yet. Zero is the unarmed state, so an
    /// unstaged boot pays one relaxed load per event and no clock read.
    pub fn answered() -> bool {
        let until = HELD_UNTIL.load(Ordering::Relaxed);
        until == 0 || crate::clock::nanos_since_boot() >= until
    }
}

/// Stage one climb of the recovery ladder to run its transfers unwaited, once:
/// a device that answers nothing, on any rung — staged because nothing on the
/// host side stops answering EP0 on its own.
#[cfg(feature = "boot-actuators")]
pub(in crate::drivers::xhci) mod reset_break {
    use core::sync::atomic::{AtomicBool, Ordering};

    static UNSPENT: AtomicBool = AtomicBool::new(true);
    static ACTIVE: AtomicBool = AtomicBool::new(false);

    /// `true` means this climb is the staged one and must call [`end`] on its
    /// way out.
    pub fn begin() -> bool {
        if !crate::actuator::usb_reset_break() || !UNSPENT.swap(false, Ordering::Relaxed) {
            return false;
        }
        ACTIVE.store(true, Ordering::Relaxed);
        true
    }

    pub fn end() {
        ACTIVE.store(false, Ordering::Relaxed);
    }

    /// Only the staged climb's own transfers can see this set.
    pub fn active() -> bool {
        ACTIVE.load(Ordering::Relaxed)
    }
}

/// What the port rung's reset is made for, in its line.
const RECOVERING: &str = "recovering";

/// Hold the port rung's first reset, once, until the port reads empty: QEMU
/// cannot move a device off its port on a reset, so the host takes it off and
/// plugs the same backing in on another port, which is T14 run 79's stick
/// leaving the USB2 half for the USB3 one.
#[cfg(feature = "boot-actuators")]
pub(in crate::drivers::xhci) mod reset_moves {
    use core::sync::atomic::{AtomicBool, Ordering};

    static UNSPENT: AtomicBool = AtomicBool::new(true);

    /// What the held reset says, which the host acts on.
    pub const HELD: &str = "is held empty for the host to move its device (usb-reset-moves)";

    /// The cue the host moves the device on, written to the console directly:
    /// the record above reaches it only when `klogd` runs, which it may not
    /// while this CPU spins in the rung, and a cue that arrives after the
    /// rung's bound stages a device that left too late.
    const MOVE_NOW: &[u8] = b"usb-reset-moves: move the device now\n";

    pub fn take() -> bool {
        crate::actuator::usb_reset_moves() && UNSPENT.swap(false, Ordering::Relaxed)
    }

    pub fn cue() {
        crate::drivers::serial::BackendGuard::lock().write_raw(MOVE_NOW);
    }
}

/// Make the controller's transfer account and the device's CSW residue
/// disagree by [`SHORT_BY`] bytes, once — only the residue is the injection's.
#[cfg(feature = "boot-actuators")]
pub(in crate::drivers::xhci) mod short_read {
    use core::sync::atomic::{AtomicBool, Ordering};

    use super::Quiet;
    use crate::mm::Dma;

    /// Bytes held back at the buffer tail: one 512-byte sector of the
    /// 4096-byte block a read asks for.
    pub const SHORT_BY: u32 = 512;

    static ARMED: AtomicBool = AtomicBool::new(false);

    pub fn arm() {
        if !crate::actuator::usb_short_read() {
            return;
        }
        ARMED.store(true, Ordering::Relaxed);
    }

    /// The tail of a data buffer, held out of the way of the transfer about to
    /// run over it.
    pub struct Held {
        at: usize,
        bytes: [u8; SHORT_BY as usize],
    }

    /// Copy the last [`SHORT_BY`] bytes out, if this is the transfer asked for.
    pub fn hold(dma: Dma<'static>, at: usize, len: u32, eligible: bool) -> Option<Held> {
        if !eligible || len < SHORT_BY || !ARMED.swap(false, Ordering::Relaxed) {
            return None;
        }
        let at = at + (len - SHORT_BY) as usize;
        let mut bytes = [0u8; SHORT_BY as usize];
        dma.copy_to(at, &mut bytes);
        Some(Held { at, bytes })
    }

    /// Put it back, and add the bytes it covers to the controller's residue.
    pub fn release(
        dma: Dma<'static>,
        held: Option<Held>,
        completion: Result<(u32, u32), Quiet>,
    ) -> Result<(u32, u32), Quiet> {
        let Some(held) = held else { return completion };
        let (code, residue) = completion?;
        dma.copy_from(held.at, &held.bytes);
        Ok((code, residue + SHORT_BY))
    }
}

/// Faults staged on the next few commands, from `usb_gate` immediately before
/// the operation they are taken inside, so each lands on a known disk and a
/// known command.
#[cfg(feature = "boot-actuators")]
pub(in crate::drivers::xhci) mod staged {
    use core::sync::atomic::{AtomicU16, AtomicU8, Ordering};

    /// What a staged command does instead of going out well formed.
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub enum Fault {
        /// The CBW goes out with a signature no device accepts. BOT §6.6.1 has
        /// the device STALL the Bulk-In and either STALL the Bulk-Out or take
        /// and discard what it is sent; QEMU's `usb-storage` STALLs the
        /// Bulk-Out alone, so the break finds the Bulk-In Running and the
        /// Bulk-Out Halted.
        BadSignature = 1,
        /// No CBW goes out, so the device is asked for bytes it holds no
        /// command for, which QEMU's `usb-storage` answers with a STALL of the
        /// Bulk-In: the break finds the Bulk-In Halted and the Bulk-Out Running.
        NoCbw = 2,
        /// Nothing is queued and the command ends as one whose port read
        /// disconnected mid-wait does.
        PortGone = 3,
        /// Nothing is queued, and the command ends as one the device never
        /// answered does: its wait runs to the end of what the call allows it.
        /// The device is sent nothing it could mistake.
        Unanswered = 4,
    }

    static LEFT: AtomicU8 = AtomicU8::new(0);
    static FAULT: AtomicU8 = AtomicU8::new(0);
    /// The one opcode the faults land on, or [`ANY`].
    static ONLY: AtomicU16 = AtomicU16::new(ANY);
    const ANY: u16 = u16::MAX;

    /// Stage `n` faults, starting with the next command — or, with `only`, the
    /// next command carrying that opcode, whichever disk issues it.
    pub fn arm(n: u8, fault: Fault, only: Option<u8>) {
        FAULT.store(fault as u8, Ordering::Relaxed);
        ONLY.store(only.map_or(ANY, u16::from), Ordering::Relaxed);
        // Last and `Release`: the disk that takes a fault may be on another CPU,
        // and what it takes has to be what was staged with the count it saw.
        LEFT.store(n, Ordering::Release);
    }

    /// Take back whatever was staged and never taken, and say how many: a
    /// fault left armed past the operation it was staged for lands on whichever
    /// disk issues the next command.
    pub fn disarm() -> u8 {
        LEFT.swap(0, Ordering::Relaxed)
    }

    /// INQUIRY faults a later bind stages on itself; a bind is no operation of
    /// the gate's, so the gate can neither stage around one nor disarm after it.
    static NEXT_BIND: AtomicU8 = AtomicU8::new(0);
    pub const INQUIRY: u8 = 0x12;

    pub fn on_a_later_bind(n: u8) {
        NEXT_BIND.store(n, Ordering::Relaxed);
    }

    /// Called by a bind before its first command: how many faults it staged.
    pub fn bind_begins() -> u8 {
        let n = NEXT_BIND.swap(0, Ordering::Relaxed);
        if n > 0 {
            arm(n, Fault::BadSignature, Some(INQUIRY));
        }
        n
    }

    /// Class resets whose TEST UNIT READY goes out with a bad signature, staged
    /// apart from [`LEFT`]: the rung it lands in is one a fault of that count
    /// began, inside the same operation.
    static PROBES: AtomicU8 = AtomicU8::new(0);

    pub fn arm_probes(n: u8) {
        PROBES.store(n, Ordering::Relaxed);
    }

    /// As [`disarm`], for the probes.
    pub fn disarm_probes() -> u8 {
        PROBES.swap(0, Ordering::Relaxed)
    }

    /// The fault the TEST UNIT READY `rung` is about to end on was staged with.
    pub fn take_probe(rung: super::Rung) -> Option<Fault> {
        if crate::actuator::usb_transport_offline() {
            return Some(Fault::Unanswered);
        }
        if rung != super::Rung::ClassReset {
            return None;
        }
        PROBES
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |left| left.checked_sub(1))
            .ok()
            .map(|_| Fault::BadSignature)
    }

    /// The fault the command about to go out was staged with, if any.
    pub fn take(opcode: u8) -> Option<Fault> {
        // The count first and `Acquire`, pairing with `arm`'s `Release`: the
        // opcode and the fault read after it are the ones staged with it.
        let mut left = LEFT.load(Ordering::Acquire);
        if left == 0 {
            return None;
        }
        let only = ONLY.load(Ordering::Relaxed);
        if only != ANY && only != u16::from(opcode) {
            return None;
        }
        loop {
            let less = left.checked_sub(1)?;
            match LEFT.compare_exchange_weak(left, less, Ordering::Acquire, Ordering::Acquire) {
                Ok(_) => break,
                Err(now) => left = now,
            }
        }
        match FAULT.load(Ordering::Relaxed) {
            1 => Some(Fault::BadSignature),
            2 => Some(Fault::NoCbw),
            3 => Some(Fault::PortGone),
            4 => Some(Fault::Unanswered),
            _ => None,
        }
    }
}

/// The one line a device's refusal produces, wherever it is noticed — one
/// function so per-caller wording never obscures what the device said.
fn log_refusal(cdb: &Cdb, sense: Sense) {
    log!("usb-storage: SCSI {:#04x} failed, sense {sense}", cdb.opcode());
}

/// The sense a test actuator makes SYNCHRONIZE CACHE answer with, or `None`
/// on a shipped kernel. ILLEGAL REQUEST/INVALID COMMAND OPERATION CODE must
/// not fail the caller; HARDWARE ERROR/INTERNAL TARGET FAILURE must.
fn flush_sense() -> Option<Sense> {
    if crate::actuator::usb_flush_unimplemented() {
        Some(Sense { key: 0x05, asc: 0x20, ascq: 0x00 })
    } else if crate::actuator::usb_flush_fails() {
        Some(Sense { key: 0x04, asc: 0x44, ascq: 0x00 })
    } else {
        None
    }
}

/// A bulk transfer's completion, as the round trip hears it.
fn completed(completion: Result<(u32, u32), Quiet>) -> bot::Answer<Quiet> {
    match completion {
        Ok((code, residue)) => bot::Answer::Moved { code, residue },
        Err(Quiet::Gone) => bot::Answer::Gone,
        Err(why) => bot::Answer::Silent(why),
    }
}

/// What a caller above the disk is told.
fn block_error(fail: Fail) -> BlockError {
    match fail {
        Fail::Device => BlockError::Device,
        Fail::Budget => BlockError::BudgetExpired,
    }
}

/// Snapshot of `dev.block`'s address and value at the top of one round
/// trip, to catch a write to `MscDevice` from outside the driver while a
/// phase is waiting.
#[cfg(feature = "stack-witness")]
#[derive(Clone, Copy)]
struct BlockWitness {
    at: u64,
    was: usize,
}

#[cfg(feature = "stack-witness")]
fn block_witness(dev: &MscDevice) -> BlockWitness {
    BlockWitness { at: &raw const dev.block as u64, was: dev.block }
}

/// Nothing in this driver writes `block` after `bind` hands the device
/// over, so any difference here is a write from outside it.
#[cfg(feature = "stack-witness")]
fn block_witness_holds(dev: &MscDevice, entered: BlockWitness) {
    let at = &raw const dev.block as u64;
    if at == entered.at && dev.block == entered.was {
        return;
    }
    // SAFETY: driver code runs on the CPU whose GS base is its own `PerCpu`.
    let (top, _) = unsafe { crate::arch::percpu::entry_stacks() };
    panic!(
        "USB BOT WITNESS: MscDevice::block changed inside one round trip — the field at \
         {at:#018x} held {:#018x} and now holds {:#018x} (the frame moved by {}). This CPU's \
         Ring 3 entry stack is {top:#018x}, so the field stands {} bytes below it and the \
         running rsp is {:#018x}. `with_storage` copies the device onto this stack, so a \
         kernel text value here is a return address something else pushed.",
        entered.was,
        dev.block,
        at.wrapping_sub(entered.at) as i64,
        top.wrapping_sub(at) as i64,
        crate::arch::cpu::stack_pointer(),
    );
}

/// Which way a block transfer moves, so one loop serves both without a
/// `&[u8]` pretending to be a `&mut [u8]`.
enum Host<'a> {
    Into(&'a mut [u8]),
    From(&'a [u8]),
}

impl Host<'_> {
    fn len(&self) -> usize {
        match self {
            Self::Into(b) => b.len(),
            Self::From(b) => b.len(),
        }
    }
}

impl XhciController {
    /// Run `f` against the disk at `at`, writing state back regardless of
    /// outcome; `None` if no disk is there.
    fn with_storage<R>(
        &mut self,
        at: usize,
        f: impl FnOnce(&mut Self, &mut Disk) -> R,
    ) -> Option<R> {
        let mut disk = self.msc.get(at)?.disk?;
        let out = f(self, &mut disk);
        self.msc[at].disk = Some(disk);
        if disk.dev.left {
            self.hold_for_return(at, "its port read empty");
        }
        Some(out)
    }

    /// One of three operation entry points: recovers the caller's deadline
    /// via [`Operation::deadline`], never done again below this call.
    pub(super) fn msc_read(&mut self, at: usize, lba: u64, count: u32, buf: &mut [u8]) -> BlockResult {
        let until = Operation::deadline();
        self.with_storage(at, |ctrl, disk| {
            ctrl.transfer_blocks(&mut disk.dev, lba, count, Host::Into(buf), until)
        })
        // No disk under this index is a device fact, never a budget.
        .unwrap_or(Err(BlockError::Device))
    }

    pub(super) fn msc_write(&mut self, at: usize, lba: u64, count: u32, buf: &[u8]) -> BlockResult {
        let until = Operation::deadline();
        self.with_storage(at, |ctrl, disk| {
            ctrl.transfer_blocks(&mut disk.dev, lba, count, Host::From(buf), until)
        })
        .unwrap_or(Err(BlockError::Device))
    }

    pub(super) fn msc_flush(&mut self, at: usize) -> BlockResult {
        let until = Operation::deadline();
        self.with_storage(at, |ctrl, disk| {
            let number = disk.index;
            let dev = &mut disk.dev;
            if dev.failed {
                return Err(BlockError::Device);
            }
            // **The latch decides before the command is built, not after it is
            // refused.** A device that has answered INVALID COMMAND OPERATION
            // CODE once answers it every time, and issuing anyway costs a full
            // Bulk-Only round trip plus a REQUEST SENSE — under `XHCI`, with
            // preemption off, on the device the log is going to — for every
            // `logd` batch on a stick like the T14's.
            if dev.no_write_cache {
                return Ok(());
            }
            let cdb = Cdb::SYNCHRONIZE_CACHE;
            let issued = ctrl.scsi(dev, &cdb, None, until);
            let reply = flush_sense().map_or(issued, Reply::Refused);
            match scsi::flushed(reply) {
                Flushed::NoCache => {
                    dev.no_write_cache = true;
                    log!("usb-storage: disk {number} does not implement SYNCHRONIZE CACHE \
                         (sense 0x05/0x20/0x00); its writes are durable once they complete");
                    Ok(())
                }
                Flushed::Emptied => {
                    dev.debt.flushed();
                    #[cfg(feature = "boot-actuators")]
                    transport_break::flushed();
                    Ok(())
                }
                Flushed::Refused(sense) => {
                    log_refusal(&cdb, sense);
                    Err(BlockError::Device)
                }
                // Unlogged: `scsi` already named a budget, and a line here
                // would itself be the next flush.
                Flushed::Ended(fail) => Err(block_error(fail)),
            }
        })
        .unwrap_or(Err(BlockError::Device))
    }

    /// Move `count` 4 KiB blocks; `until` bounds the whole call, not one
    /// command — [`Self::scsi`] refuses to start a command past the deadline.
    fn transfer_blocks(
        &mut self,
        dev: &mut MscDevice,
        lba: u64,
        count: u32,
        mut host: Host<'_>,
        until: Deadline,
    ) -> BlockResult {
        let write = matches!(host, Host::From(_));
        // Caller/trait mismatch is a kernel bug: fail-fast. Below this line
        // every check is about device numbers and gets a refusal instead.
        assert_eq!(host.len(), count as usize * HOST_BLOCK as usize);
        if dev.failed {
            return Err(BlockError::Device);
        }
        let mut transfer = match Transfer::new(lba, count, write, &dev.geometry, MSC_MAX_BLOCKS) {
            Ok(transfer) => transfer,
            Err(past) => {
                log!("usb-storage: {lba}+{count} is past the {} blocks this disk has", past.blocks);
                return Err(BlockError::Device);
            }
        };

        let dma = self.dma();
        let data = dma.subview(dev.block + MSC_DATA, MSC_DATA_LEN);
        while let Some(batch) = transfer.next() {
            let (bytes, offset) = (batch.bytes, batch.offset);
            if let Host::From(src) = &host {
                dma.copy_from(dev.block + MSC_DATA, &src[offset..offset + bytes]);
            }
            let reply = self.scsi(dev, &batch.cdb, Some(data.subview(0, bytes)), until);
            let moved = transfer.answered(&batch, reply);
            if moved.reported() {
                dev.wrote(write);
            }
            match moved {
                Moved::Whole => {}
                Moved::Short { delivered } => {
                    log!("usb-storage: {delivered} of {bytes} B at block {}", batch.block);
                    return Err(BlockError::Device);
                }
                Moved::Refused(sense) => {
                    log_refusal(&batch.cdb, sense);
                    return Err(BlockError::Device);
                }
                Moved::Ended(fail) => return Err(block_error(fail)),
            }
            if let Host::Into(dst) = &mut host {
                dma.copy_to(dev.block + MSC_DATA, &mut dst[offset..offset + bytes]);
            }
        }
        Ok(())
    }

    /// One SCSI command with transport recovery applied and re-issued;
    /// every CDB here is idempotent, so a re-issue is genuine. `until` is
    /// checked only here, before a command starts and never before the one a
    /// rung that took sends again — it costs the device nothing (no
    /// TRB on a ring, no phase half done); checking inside [`Self::bot`]
    /// would abandon a transfer the device is still going to answer.
    ///
    /// **A break is counted against the device and not against this call**,
    /// so the run of breaks [`MAX_TRANSPORT_BREAKS`] bounds is the run the
    /// device has produced, across however many operations the caller spent
    /// on it; a completed round trip is what ends the run, and says so — the
    /// call the break happened in may have ended on its budget before it
    /// could ask again.
    ///
    /// **Everything the call does once its transport has broken is bounded,
    /// rung by rung** ([`AFTER_BREAK`]): opened by the first break and closed
    /// by whoever the call is — [`served`] for a block operation, the bind for
    /// each of its commands — so a later command of the same operation spends
    /// what the break left it, and no call inherits another's.
    fn scsi(&mut self, dev: &mut MscDevice, cdb: &Cdb, data: DataPhase, until: Deadline) -> Reply {
        let opcode = cdb.opcode();
        let data_out = data.is_some() && !cdb.data_in();
        // Named per line so a multi-disk boot's retry log attributes to the
        // right disk.
        let slot = self.slot(dev.slot_id);
        loop {
            // Not `Broken` either way: nothing was issued and `dev.failed` stays
            // untouched. The command sent again after a rung that took is the
            // call's to decide and not the operation's, whose budget the break
            // may have spent (`toyos_xhci::call`).
            match self.after_break.issue(crate::clock::nanos_since_boot(), until.nanos()) {
                Ok(()) => {}
                Err(NotIssued::Operation) => {
                    log!("usb-storage: {slot} SCSI {opcode:#04x} not issued: {}",
                        crate::block::OPERATION);
                    return Reply::Budget;
                }
                // A command re-issued with nothing left would have every wait
                // cut at once, and count against the device a break that was
                // the budget's.
                Err(NotIssued::Call(why)) => {
                    log!("usb-storage: {slot} SCSI {opcode:#04x} not issued again: {why}");
                    return Reply::Budget;
                }
            }
            match self.bot(dev, cdb, data, Asks::Command) {
                Ok(Bot::Done { delivered }) => {
                    self.transport_came_back(dev, opcode);
                    return Reply::Ok { delivered };
                }
                Ok(Bot::Failed) => {
                    self.transport_came_back(dev, opcode);
                    return Reply::Refused(self.request_sense(dev));
                }
                // Not a transport that broke: a device that is no longer on
                // the bus. Its port's own teardown gives the slot and the pool
                // block back, and a recovery or a reset aimed at an empty port
                // would only spend their bounds.
                Err(gone @ Broke::Gone { .. }) => {
                    let broke = Told(&gone);
                    log!("usb-storage: {slot} transport broke on SCSI {opcode:#04x}: {broke}; \
                         its port's teardown takes it from here");
                    dev.failed = true;
                    dev.left = true;
                    // A hold for its device is part of this call, from the
                    // wait that saw it go.
                    self.after_break.open(self.bulk_began, AFTER_BREAK);
                    return Reply::Broken;
                }
                Err(why) => {
                    self.after_break.open(self.bulk_began, AFTER_BREAK);
                    let broke = Told(&why);
                    log!("usb-storage: {slot} transport broke on SCSI {opcode:#04x}: {broke}; \
                         break {} of {MAX_TRANSPORT_BREAKS} running", dev.run.breaks().saturating_add(1));
                    if !self.climb_until_in_step(dev, why.event(), why.left(data_out)) {
                        return Reply::Broken;
                    }
                }
            }
        }
    }

    /// What one break costs: it is counted, and the device climbs the next rung
    /// of `toyos_xhci::ladder` — above every rung this run of breaks has
    /// climbed, and never below where the break itself enters. A rung that did
    /// not verify is the next break, so the climb goes on inside this call
    /// until the device has answered under its own tag — `true`, the only
    /// device the caller's command goes to again — or is offline, `false`.
    ///
    /// **Each rung is entered on its own bound** (`toyos_xhci::call`), so no
    /// rung is starved by what the ones before it spent.
    ///
    /// `broke` is the transfer event that ended the round trip, where one did,
    /// and `left` where it left the device.
    fn climb_until_in_step(
        &mut self,
        dev: &mut MscDevice,
        broke: Option<(Pipe, u32)>,
        left: Left,
    ) -> bool {
        #[cfg(feature = "boot-actuators")]
        let staged = reset_break::begin();
        let in_step = self.climb(dev, broke, left);
        #[cfg(feature = "boot-actuators")]
        if staged {
            reset_break::end();
        }
        in_step
    }

    fn climb(&mut self, dev: &mut MscDevice, mut broke: Option<(Pipe, u32)>, mut left: Left) -> bool {
        let slot = self.slot(dev.slot_id);
        loop {
            let ladder::Climb { rung, skips_class_reset } = dev.run.broke(left);
            if skips_class_reset {
                log!("usb-storage: {slot} is owed the data of the command that broke, so nothing \
                     can be asked of it on the Bulk-Out: its port is reset with no class reset \
                     before it");
            }
            self.after_break.enter(rung, crate::clock::nanos_since_boot());
            let climbed = match rung {
                Rung::ClassReset => self.reset_recovery(dev, broke),
                Rung::PortReset => self.port_reset_recovery(dev, broke),
                Rung::Offline => {
                    log!("usb-storage: {slot} broke {} times running; its port reset did not \
                         bring the transport back", dev.run.breaks());
                    self.take_offline(dev, broke);
                    return false;
                }
            };
            match climbed {
                Climbed::InStep => {
                    self.after_break.took(rung);
                    return true;
                }
                Climbed::OutOfStep(why) => {
                    log!("usb-storage: {slot} transport broke on the {}'s TEST UNIT READY: {}; \
                         break {} of {MAX_TRANSPORT_BREAKS} running",
                        rung.named(), Told(&why), dev.run.breaks().saturating_add(1));
                    broke = why.event();
                }
                // As a round trip whose port read disconnected mid-wait: a
                // reset aimed at an empty port would only spend its bound.
                Climbed::Gone => {
                    dev.failed = true;
                    dev.left = true;
                    return false;
                }
                Climbed::Failed => {
                    log!("usb-storage: {slot} the {} was not answered; break {} of \
                         {MAX_TRANSPORT_BREAKS} running", rung.named(), dev.run.breaks().saturating_add(1));
                    // The event is spent: the rung has commanded the pair
                    // since, and only the fields speak for it now.
                    broke = None;
                }
            }
            // A rung's own TEST UNIT READY has no data phase to be left in.
            left = Left::Elsewhere;
        }
    }

    /// A round trip completed: the run of breaks is over, and the log says so
    /// where there was one.
    fn transport_came_back(&mut self, dev: &mut MscDevice, opcode: u8) {
        let breaks = dev.run.over();
        if breaks > 0 {
            log!("usb-storage: {} SCSI {opcode:#04x} completed after {breaks} break(s) running; \
                 the transport came back and the count is cleared", self.slot(dev.slot_id));
        }
    }

    /// The ladder's last rung: the device did not answer after a port reset, or
    /// kept breaking after one that it did. Every operation on the disk is
    /// refused from here.
    ///
    /// **The last thing the device is sent is a reset**, whichever step it
    /// stopped answering at: a device left holding a command it will never be
    /// asked to finish is one the next host may not be able to enumerate. So
    /// the port is reset whatever the endpoints could be taken to, and then the
    /// controller is told (Reset Device, xHCI 1.2 §4.6.11), which leaves the
    /// slot in Default with every endpoint but the control endpoint Disabled —
    /// what the device now is, and a slot Disable Slot is defined over
    /// (§4.6.4's note). Nothing is sent after it.
    ///
    /// **On its own bound**, entered by the climb, so the quiesce and the
    /// reset's settle run whatever the rungs before spent.
    ///
    /// **The slot is given back by whoever the port says holds it.** A bound
    /// disk's goes back from the poll ([`MscDevice::take_slot_owed`]). A disk
    /// still inside its bind holds no slot as far as its port knows: [`bind`]
    /// answers with where the slot goes, and the enumeration that failed either
    /// gives it back, once, or hands it to the port.
    fn take_offline(&mut self, dev: &mut MscDevice, broke: Option<(Pipe, u32)>) {
        dev.failed = true;
        let slot = self.slot(dev.slot_id);
        let in_state = self.endpoint_state(dev.dev_block, dev.in_dci());
        let out_state = self.endpoint_state(dev.dev_block, dev.out_dci());
        let stopped = reset_recovery::nothing_to_stop(in_state, out_state)
            || self.quiesce_bulk_pair(dev, broke, "stopping it before its port is reset");
        let reset = self.reset_port(dev, "taking it offline");
        dev.left |= reset == AfterReset::Left;
        // A Context State Error is a slot already in Default (§4.6.11): the
        // port rung got as far as its own Reset Device and no further.
        let told = reset.finished()
            && matches!(
                self.command_code(reset_device_trb(dev.slot_id), "Reset Device"),
                Some(CC_SUCCESS | CC_CONTEXT_STATE_ERROR)
            );
        let goes = reset_recovery::slot_after_offline(stopped || told);
        dev.slot_goes = Some(goes);
        log!(
            "usb-storage: {slot} is offline: both bulk endpoints Stopped={stopped}, port {} \
             reset={} and nothing sent after it, Reset Device={told}, {}; every operation on it \
             is refused from here",
            u32::from(dev.port_idx) + 1,
            reset.finished(),
            match goes {
                SlotGoes::Back => "its slot goes back to the controller",
                SlotGoes::WithTheUnplug => "its slot is kept until the unplug",
            }
        );
    }

    /// The most reset this device's port has, waited for on what the rung it is
    /// part of may still spend, its change flags consumed as an enumeration's
    /// are so the port machine reads none of them as a replug. Says what the
    /// port read before and after, and answers with what the reset left.
    ///
    /// The write is made whatever is left of the bound: it costs nothing, and
    /// it is what leaves the device in its Default state.
    fn reset_port(&mut self, dev: &mut MscDevice, why: &str) -> AfterReset {
        let port_idx = dev.port_idx;
        let before = self.read_portsc(port_idx);
        let protocol = self.protocols.of(port_idx);
        let kind = port::offline_reset(protocol);
        // A port that reads empty, or connected across a gap, no longer holds
        // the device this is about: its teardown owns what is left.
        let here = before.connected() && !before.connect_changed();
        let finished = here && {
            self.write_portsc(port_idx, port::reset_write(kind, before));
            dev.reset_at = Some(crate::clock::nanos_since_boot());
            self.settles_within_call(|| self.read_portsc(port_idx).reset_finished())
        };
        #[cfg(feature = "boot-actuators")]
        if finished && why == RECOVERING && reset_moves::take() {
            log!("xHCI: {} port {} {}", self.slot(dev.slot_id), u32::from(port_idx) + 1,
                reset_moves::HELD);
            reset_moves::cue();
            let _ = self.settles_within_call(|| !self.read_portsc(port_idx).connected());
        }
        let after = self.read_portsc(port_idx);
        if finished {
            self.write_portsc(port_idx, port::enumeration_ack(Some(kind), after));
        }
        let left = ladder::after_reset(
            finished,
            here && after.connected(),
            after.enabled(),
            after.speed(),
            dev.enumerated.speed,
        );
        log!(
            "xHCI: {} port {} reset while {why} ({} on {}): PORTSC {:#010x} then {:#010x}, link \
             {:?}, speed {}: {left}",
            self.slot(dev.slot_id),
            u32::from(port_idx) + 1,
            match kind {
                port::Reset::Hot => "hot",
                port::Reset::Warm => "warm",
            },
            match protocol {
                Some(toyos_xhci::Protocol::Usb2) => "a USB2 port",
                Some(toyos_xhci::Protocol::Usb3) => "a USB3 port",
                None => "a port of no named protocol",
            },
            before.raw(),
            after.raw(),
            after.link_state(),
            after.speed(),
        );
        left
    }

    /// The ladder's second rung: the port reset, and the enumeration a reset
    /// owes (xHCI 1.2 §4.19.5), on the slot, the pool block and the disk number
    /// the device already has — so a mount on it carries on. The steps and
    /// their order are `ladder::PORT_RESET`'s; this takes them, one blocking
    /// command or control transfer at a time, and ends on the device's answer
    /// to TEST UNIT READY.
    fn port_reset_recovery(&mut self, dev: &mut MscDevice, broke: Option<(Pipe, u32)>) -> Climbed {
        let slot = self.slot(dev.slot_id);
        for step in ladder::PORT_RESET {
            let took = match step {
                PortStep::Quiesce => {
                    self.quiesce_bulk_pair(dev, broke, "stopping it before its port is reset")
                }
                PortStep::Reset => match self.reset_port(dev, RECOVERING) {
                    AfterReset::Enumerate => true,
                    AfterReset::Left => return Climbed::Gone,
                    // `reset_port` has said which.
                    AfterReset::NeverFinished
                    | AfterReset::NotEnabled
                    | AfterReset::SpeedChanged { .. } => false,
                },
                PortStep::Settle => {
                    let _ = crate::clock::settles(
                        self.after_break
                            .wait_left(crate::clock::nanos_since_boot(), ladder::RESET_RECOVERY_NS),
                        || false,
                    );
                    true
                }
                PortStep::ResetDevice => {
                    self.run_command(reset_device_trb(dev.slot_id), "Reset Device")
                }
                PortStep::Address => {
                    let trb = address_again_trb(self, dev);
                    self.run_command(trb, "Address Device (after the port reset)")
                }
                PortStep::Configure => {
                    let (slot_id, block) = (dev.slot_id, dev.dev_block);
                    let value = u16::from(dev.enumerated.configuration);
                    let set = self
                        .control_transfer(slot_id, block, &mut dev.ep0_ring, 0x00, 0x09, value, 0, None, 0);
                    if !set.done() {
                        log!("usb-storage: {slot} would not take SET_CONFIGURATION({value}) after \
                             its port reset: {set}");
                    }
                    set.done()
                }
                PortStep::AddEndpoints => self.configure_bulk_pair(
                    dev,
                    configure::Configure::First {
                        speed: dev.enumerated.speed,
                        port: dev.port_idx + 1,
                    },
                ),
            };
            if !took && ladder::ends_the_rung(step) {
                return Climbed::Failed;
            }
        }
        match self.bot(dev, &Cdb::TEST_UNIT_READY, None, Asks::Verification(Rung::PortReset)) {
            Ok(answer) => {
                log!("usb-storage: {slot} the port reset took: addressed and configured again, the \
                     device answered TEST UNIT READY under its own tag {:#x}", dev.tag);
                self.take_held_sense(dev, answer);
                Climbed::InStep
            }
            Err(why) => Climbed::OutOfStep(why),
        }
    }

    /// Status 1 on a rung's TEST UNIT READY is sense the device holds for
    /// whoever asks next, which would otherwise be the command the rung is for.
    fn take_held_sense(&mut self, dev: &mut MscDevice, answer: Bot) {
        if matches!(answer, Bot::Failed) {
            let sense = self.request_sense(dev);
            log!("usb-storage: {} held sense {sense} after its recovery", self.slot(dev.slot_id));
        }
    }

    /// REQUEST SENSE through `bot` directly, so it cannot recurse into asking
    /// for sense about itself.
    fn request_sense(&mut self, dev: &mut MscDevice) -> Sense {
        let dma = self.dma();
        let scratch = dma.subview(dev.block + MSC_SCRATCH, MSC_SCRATCH_LEN);
        scratch.zero();
        let region = scratch.subview(0, scsi::SENSE_BYTES);
        match self.bot(dev, &Cdb::REQUEST_SENSE, Some(region), Asks::Command) {
            Ok(Bot::Done { delivered }) => {
                let mut response = [0u8; scsi::SENSE_BYTES];
                dma.copy_to(dev.block + MSC_SCRATCH, &mut response);
                Sense::of(&response, delivered)
            }
            _ => Sense::NONE,
        }
    }

    /// One Bulk-Only round trip, as `toyos_xhci::bot::RoundTrip` asks for it:
    /// each transfer queued and waited for in place.
    fn bot(&mut self, dev: &mut MscDevice, cdb: &Cdb, data: DataPhase, asks: Asks) -> Result<Bot, Broke> {
        crate::block::census::command_issued();
        let data_in = cdb.data_in();
        // The length the device is told to move is the region's own, so the
        // only bound left to state is this driver's largest transfer.
        let (data_phys, data_len) = match data {
            Some(region) => {
                assert!(
                    region.size() <= MSC_DATA_LEN,
                    "usb-storage: a {} B data phase, past the {MSC_DATA_LEN} B this driver rings",
                    region.size(),
                );
                (region.device_addr(), region.size() as u32)
            }
            None => (0, 0),
        };

        #[cfg(feature = "boot-actuators")]
        let staged = match asks {
            Asks::Verification(rung) => staged::take_probe(rung),
            Asks::Command => staged::take(cdb.opcode()),
        };
        #[cfg(not(feature = "boot-actuators"))]
        let _ = asks;
        #[cfg(feature = "boot-actuators")]
        if staged == Some(staged::Fault::Unanswered) {
            let began = crate::clock::nanos_since_boot();
            // The staged wait is the wait a break opens the call from.
            self.bulk_began = began;
            let _ = self.settles_within_call(|| false);
            let now = crate::clock::nanos_since_boot();
            let cut = self.after_break.cut(began, now, super::super::USB_TIMEOUT_NS);
            let why = if cut { Quiet::Spent } else { Quiet::Elapsed };
            return Err(Broke::Silence { phase: Phase::Status, why });
        }

        let dma = self.dma();
        let tag = dev.next_tag();
        #[cfg(feature = "stack-witness")]
        let entered_with = block_witness(dev);
        dma.copy_from(dev.block + MSC_CBW, &bot::cbw(tag, data_len, cdb));
        #[cfg(feature = "boot-actuators")]
        if staged == Some(staged::Fault::BadSignature) {
            dma.copy_from(dev.block + MSC_CBW, &[0; 4]);
        }

        // **From here the device is one this kernel has spoken a command to**,
        // and stays one until the CSW below is in hand: a reset between any two
        // phases leaves it waiting, which is what `stop::settle_commands`
        // finishes. Opened before the CBW's transfer is queued, so nothing
        // reaches the controller unpublished.
        let open = stop::OpenCommand::begin(stop::Device {
            block: dma.subview(dev.block, MSC_STRIDE),
            slot: dev.slot_id,
            in_dci: dev.in_dci(),
            out_dci: dev.out_dci(),
            ctx: dma.subview(dev.dev_block + super::super::DEV_OUT_CTX, 32 * self.context_size),
            ctx_size: self.context_size as u32,
            data_in,
        });
        #[cfg(feature = "boot-actuators")]
        let write = cdb.opcode() == scsi::WRITE_10;

        let (mut trip, mut act) = RoundTrip::begin(tag, data_len, cdb);
        loop {
            let answer = match act {
                bot::Act::Command => {
                    #[cfg(feature = "boot-actuators")]
                    let staged_answer = match staged {
                        Some(staged::Fault::NoCbw) => Some(bot::Answer::Moved { code: CC_SUCCESS, residue: 0 }),
                        Some(staged::Fault::PortGone) => Some(completed(Err(Quiet::Gone))),
                        _ => None,
                    };
                    #[cfg(not(feature = "boot-actuators"))]
                    let staged_answer = None;
                    match staged_answer {
                        Some(answer) => answer,
                        None => {
                            let cbw_phys = dma.device_addr() + (dev.block + MSC_CBW) as u64;
                            completed(self.bulk(dev, false, cbw_phys, CBW_LEN as u32, Phase::Command, &open))
                        }
                    }
                }
                bot::Act::Data(pipe) => {
                    open.at(Phase::DataOwed, &dev.in_ring, &dev.out_ring);
                    #[cfg(feature = "boot-actuators")]
                    if write {
                        transport_break::arm(dev.owes_a_flush());
                        mid_write::wedge_if_staged(Phase::DataOwed);
                    }
                    #[cfg(feature = "boot-actuators")]
                    let held = short_read::hold(
                        dma,
                        (data_phys - dma.device_addr()) as usize,
                        data_len,
                        data_in && cdb.opcode() == scsi::READ_10,
                    );
                    let completion = self.bulk(dev, pipe == Pipe::In, data_phys, data_len, Phase::Data, &open);
                    #[cfg(feature = "boot-actuators")]
                    let completion = short_read::release(dma, held, completion);
                    completed(completion)
                }
                bot::Act::Status => {
                    open.at(Phase::StatusOwed, &dev.in_ring, &dev.out_ring);
                    #[cfg(feature = "boot-actuators")]
                    if data_len > 0 && write {
                        mid_write::wedge_if_staged(Phase::StatusOwed);
                    }
                    super::super::zero_dma(dma, dev.block + MSC_CSW, CSW_LEN);
                    let csw_phys = dma.device_addr() + (dev.block + MSC_CSW) as u64;
                    completed(self.bulk(dev, true, csw_phys, CSW_LEN as u32, Phase::Status, &open))
                }
                bot::Act::Restart { pipe, then } => {
                    let took = self.restart_bulk(dev, pipe == Pipe::In);
                    // The recovery rebuilt the ring, so the point the account
                    // reads is republished before anything else reaches the
                    // controller.
                    if took {
                        open.at(then, &dev.in_ring, &dev.out_ring);
                    }
                    bot::Answer::Restarted(took)
                }
            };
            match trip.answered(answer) {
                bot::Next::Act(next, then) => (trip, act) = (next, then),
                bot::Next::Csw(due) => {
                    #[cfg(feature = "stack-witness")]
                    block_witness_holds(dev, entered_with);
                    let mut csw = [0u8; CSW_LEN];
                    dma.copy_to(dev.block + MSC_CSW, &mut csw);
                    return due.judge(&csw);
                }
                bot::Next::Broke(broke) => return Err(broke),
            }
        }
    }

    /// One Normal TRB on a bulk endpoint, and its completion.
    ///
    /// `phase` is the leg of the round trip this TRB is, published between the
    /// enqueue and the doorbell.
    fn bulk(
        &mut self,
        dev: &mut MscDevice,
        in_dir: bool,
        phys: u64,
        len: u32,
        phase: Phase,
        open: &stop::OpenCommand,
    ) -> Result<(u32, u32), Quiet> {
        let (dci, ring) = if in_dir {
            (dev.in_dci(), &mut dev.in_ring)
        } else {
            (dev.out_dci(), &mut dev.out_ring)
        };
        let mut trb = Trb::ZERO;
        trb.param = phys;
        trb.status = len;
        // ISP so a device that sends less than asked reports it instead of
        // leaving the transfer outstanding, IOC so it reports at all.
        trb.control = TRB_NORMAL | (1 << 5) | (1 << 2);
        let at = ring.enqueue(trb);
        let slot = dev.slot_id;
        // Before the doorbell, so no transfer is visible to the controller
        // without a reset being able to see the ring it went on.
        open.at(phase, &dev.in_ring, &dev.out_ring);
        #[cfg(feature = "boot-actuators")]
        if phase == Phase::Data && !in_dir {
            mid_write::wedge_if_staged(Phase::Data);
        }
        self.bulk_began = crate::clock::nanos_since_boot();
        self.ring_doorbell(slot, dci);
        #[cfg(feature = "boot-actuators")]
        if transport_break::take() {
            return Err(Quiet::Staged);
        }
        self.wait_transfer(slot, dci, at)
    }

    /// One of this disk's bulk endpoints, packaged for recovery; which
    /// command is legal is [`XhciController::restart_endpoint`]'s to decide.
    fn bulk_endpoint<'a>(dev: &'a mut MscDevice, in_dir: bool) -> Restart<'a> {
        let (dci, ep_addr, ring_off) = if in_dir {
            (dev.in_dci(), dev.pair.in_ep.addr, MSC_IN_RING)
        } else {
            (dev.out_dci(), dev.pair.out_ep.addr, MSC_OUT_RING)
        };
        Restart {
            slot_id: dev.slot_id,
            ctx_block: dev.dev_block,
            dci,
            ep_addr,
            ring_at: dev.block + ring_off,
            ring: if in_dir { &mut dev.in_ring } else { &mut dev.out_ring },
            ep0_ring: &mut dev.ep0_ring,
        }
    }

    /// One of this disk's bulk endpoints, back to a state that runs TRBs.
    fn restart_bulk(&mut self, dev: &mut MscDevice, in_dir: bool) -> bool {
        self.restart_endpoint(Self::bulk_endpoint(dev, in_dir))
    }

    /// Bulk-Only Transport §5.3.4's Reset Recovery, with the commands an xHC
    /// owes ahead of it: `toyos_xhci::reset_recovery` decides the steps and
    /// this takes them, one blocking command or control transfer at a time.
    ///
    /// A command that did not take ends it, since the requests after it assume
    /// both endpoints are off their transfers; a request that did not is
    /// followed by the rest, so the device is left with both pipes cleared
    /// whatever the next rung then does with it. Either is [`Climbed::Failed`].
    ///
    /// **Then the device is asked, and only its answer says the recovery
    /// took**: TEST UNIT READY, whose status must carry that command's own tag.
    ///
    /// `broke` is the transfer event that ended the round trip, where one did.
    fn reset_recovery(&mut self, dev: &mut MscDevice, broke: Option<(Pipe, u32)>) -> Climbed {
        if !self.quiesce_bulk_pair(dev, broke, "recovering") {
            return Climbed::Failed;
        }
        let slot = self.slot(dev.slot_id);
        let mut recovered = true;
        for step in reset_recovery::AFTER_QUIESCE {
            let took = match step {
                Step::Reconfigure => self.configure_bulk_pair(dev, configure::Configure::Again),
                Step::MassStorageReset => {
                    let (slot_id, block, iface) = (dev.slot_id, dev.dev_block, dev.pair.iface_num);
                    let reset = self.control_transfer(
                        slot_id, block, &mut dev.ep0_ring, 0x21, 0xFF, 0, u16::from(iface), None, 0,
                    );
                    if !reset.done() {
                        log!("usb-storage: {slot} would not take a Bulk-Only Reset: {reset}");
                    }
                    reset.done()
                }
                Step::ClearHalt(pipe) => {
                    let ep_addr = dev.ep_addr(pipe);
                    self.clear_endpoint_halt(dev.slot_id, dev.dev_block, &mut dev.ep0_ring, ep_addr)
                }
            };
            if !took {
                recovered = false;
                if step == Step::Reconfigure {
                    break;
                }
            }
        }
        if !recovered {
            return Climbed::Failed;
        }
        match self.bot(dev, &Cdb::TEST_UNIT_READY, None, Asks::Verification(Rung::ClassReset)) {
            Ok(answer) => {
                log!("usb-storage: {slot} Reset Recovery took: the device answered TEST UNIT \
                     READY under its own tag {:#x}", dev.tag);
                self.take_held_sense(dev, answer);
                Climbed::InStep
            }
            Err(why) => Climbed::OutOfStep(why),
        }
    }

    /// Both bulk endpoints to Stopped. Every decision is
    /// `reset_recovery::Quiescing`'s — which command, what an answer means, and
    /// when to stop asking — and this issues what it is told and reports what
    /// came back; the loop ends where the plan does. `broke` is the transfer
    /// event that ended the round trip, and `why` ends the line that names each
    /// endpoint's field.
    fn quiesce_bulk_pair(
        &mut self,
        dev: &mut MscDevice,
        broke: Option<(Pipe, u32)>,
        why: &str,
    ) -> bool {
        let slot = self.slot(dev.slot_id);
        let mut plan = Quiescing::begin(broke);
        let mut said = false;
        // What each Stop Endpoint's own transfer event said of the TRB it
        // stopped inside, Bulk-In first.
        let mut cut = [None; 2];
        loop {
            let in_state = self.endpoint_state(dev.dev_block, dev.in_dci());
            let out_state = self.endpoint_state(dev.dev_block, dev.out_dci());
            if !core::mem::replace(&mut said, true) {
                log!("xHCI: {slot} endpoint {} is {in_state}, {why}", dev.in_dci());
                log!("xHCI: {slot} endpoint {} is {out_state}, {why}", dev.out_dci());
            }
            let (cmd, pipe) = match plan.look(in_state, out_state) {
                Look::Stopped => {
                    self.log_unreached(dev, cut);
                    return true;
                }
                Look::Command(cmd, pipe) => (cmd, pipe),
                Look::GaveUp(gave_up) => {
                    match gave_up {
                        GaveUp::NeedsConfigure(pipe, state) => {
                            log_unrecoverable(slot, dev.dci(pipe), state)
                        }
                        GaveUp::Refused(cmd, code) => {
                            log!("xHCI: {} failed: {}", cmd.name(), Completion(code))
                        }
                        // `command_code` has said so already.
                        GaveUp::Silent(_) => {}
                        GaveUp::Contradicted(pipe) => log!(
                            "xHCI: {slot} endpoint {} answered {} to every command its states \
                             define",
                            dev.dci(pipe),
                            Completion(CC_CONTEXT_STATE_ERROR)
                        ),
                        GaveUp::OutOfLooks => log!(
                            "xHCI: {slot} bulk pair is not Stopped after {} looks",
                            reset_recovery::MOST_LOOKS
                        ),
                    }
                    return false;
                }
            };
            let (slot_id, dci) = (dev.slot_id, dev.dci(pipe));
            let (ring, ring_at) = dev.ring_mut(pipe);
            let trb = self.recovery_trb(cmd, slot_id, dci, ring, ring_at);
            self.stopped = None;
            let code = self.command_code(trb, cmd.name());
            if let Some((_, _, code, left)) =
                self.stopped.take().filter(|(s, d, ..)| (*s, *d) == (slot_id, dci))
            {
                cut[usize::from(pipe == Pipe::Out)] = Some((code, left));
            }
            if let Answered::Moved { from } = plan.answered(code) {
                log!(
                    "xHCI: {slot} endpoint {dci} was not {from}: {} answered {}; looking again",
                    cmd.name(),
                    Completion(CC_CONTEXT_STATE_ERROR)
                );
            }
        }
    }

    /// What the controller had not reached on each Stopped pipe's ring, off the
    /// TR Dequeue Pointer it saves into the output context when the endpoint
    /// leaves Running (xHCI 1.2 §4.6.9), and what each Stop Endpoint's own
    /// transfer event said of the TRB it stopped inside: `cut` is its
    /// completion code and TRB Transfer Length — the bytes of that TRB the
    /// controller had not moved — Bulk-In first. Read and said, never acted on:
    /// how much of a transfer this driver stopped waiting for had reached the
    /// device is what the device is holding when the class reset reaches it.
    fn log_unreached(&self, dev: &MscDevice, cut: [Option<(u32, u32)>; 2]) {
        struct Left {
            dci: u8,
            unreached: Result<u16, toyos_xhci::NotOnTheRing>,
            cut: Option<(u32, u32)>,
        }
        impl core::fmt::Display for Left {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                write!(f, "endpoint {} ", self.dci)?;
                match &self.unreached {
                    Ok(unreached) => write!(f, "with {unreached} TRB(s) unreached")?,
                    Err(why) => write!(f, "with {why}")?,
                }
                match self.cut {
                    Some((code, left)) => {
                        write!(f, " and a stop event of {}, {left} B unmoved", Completion(code))
                    }
                    None => f.write_str(" and no stop event"),
                }
            }
        }
        let dma = self.dma();
        let left = |pipe: Pipe, ring: &TrbRing, cut| {
            let ctx = dev.dev_block
                + super::super::DEV_OUT_CTX
                + usize::from(dev.dci(pipe)) * self.context_size;
            // Volatile through `Dma`: the controller writes both by DMA.
            let dequeue = u64::from(dma.read::<u32>(ctx + 8))
                | (u64::from(dma.read::<u32>(ctx + 12)) << 32);
            let queued = toyos_xhci::Ring {
                base: ring.base_phys,
                trbs: super::super::RING_SIZE as u16,
                tail: ring.tail,
            };
            Left { dci: dev.dci(pipe), unreached: queued.pending(dequeue), cut }
        };
        log!(
            "xHCI: {} bulk pair Stopped: {}; {}",
            self.slot(dev.slot_id),
            left(Pipe::In, &dev.in_ring, cut[0]),
            left(Pipe::Out, &dev.out_ring, cut[1])
        );
    }

    /// Configure Endpoint making the bulk pair on fresh rings (xHCI 1.2 §4.6.6):
    /// dropped and added by a class reset, where they come back Running at
    /// their rings' first TRB with their data toggle or sequence number zeroed
    /// as the ClearFeature(ENDPOINT_HALT) the device is about to get zeroes its
    /// own; added by a port reset, whose Reset Device left them Disabled. The
    /// input context is the bind's own writer's, so the pair is described
    /// exactly as it was configured.
    ///
    /// **The controller's own word is read back and printed, not judged.** No
    /// doorbell has been rung on either ring, so an endpoint the output context
    /// calls Running is one the command added. The field lags the endpoint
    /// (§4.8.3), so a disk is not taken offline on it: the next transfer is
    /// what says whether the pair runs.
    fn configure_bulk_pair(&mut self, dev: &mut MscDevice, how: configure::Configure) -> bool {
        let dma = self.dma();
        dev.in_ring = TrbRing::init(dma.subview(dev.block + MSC_IN_RING, PAGE));
        dev.out_ring = TrbRing::init(dma.subview(dev.block + MSC_OUT_RING, PAGE));
        let input_ctx = bulk_pair_input_context(
            self,
            dev.block + MSC_INPUT_CTX,
            &dev.pair,
            &dev.in_ring,
            &dev.out_ring,
            how,
        );
        let (what, did) = match how {
            configure::Configure::Again => {
                ("Configure Endpoint (the bulk pair dropped and added)", "dropped and added")
            }
            configure::Configure::First { .. } => {
                ("Configure Endpoint (the bulk pair added after the port reset)", "added again")
            }
        };
        let mut configure = Trb::ZERO;
        configure.param = input_ctx.device_addr();
        configure.control = TRB_CONFIGURE_EP | (u32::from(dev.slot_id) << 24);
        if !self.run_command(configure, what) {
            return false;
        }
        let in_state = self.endpoint_state(dev.dev_block, dev.in_dci());
        let out_state = self.endpoint_state(dev.dev_block, dev.out_dci());
        let slot = self.slot(dev.slot_id);
        log!("xHCI: {slot} bulk pair {did}: endpoint {} is {in_state}, endpoint {} is {out_state}",
            dev.in_dci(), dev.out_dci());
        true
    }
}

/// Reset Device for `slot_id` (xHCI 1.2 §6.4.3.10): the slot and nothing else.
fn reset_device_trb(slot_id: u8) -> Trb {
    let mut trb = Trb::ZERO;
    trb.control = TRB_RESET_DEVICE | (u32::from(slot_id) << 24);
    trb
}

/// Address Device for a device the port rung has just reset, from the Default
/// state Reset Device left its slot in (§4.6.5, BSR clear): the slot context
/// the enumeration gave it, and the control endpoint resuming where its ring
/// stands. In the disk's own page, as [`bulk_pair_input_context`] is.
fn address_again_trb(ctrl: &XhciController, dev: &MscDevice) -> Trb {
    let input_ctx = super::super::zero_dma(ctrl.dma(), dev.block + MSC_INPUT_CTX, PAGE);
    ctrl.write_ctx32(input_ctx, 0, 1, 0x3); // A0 and A1: the slot and the control endpoint
    ctrl.write_ctx32(input_ctx, 1, 0, (u32::from(dev.enumerated.speed) << 20) | (1 << 27));
    ctrl.write_ctx32(input_ctx, 1, 1, (u32::from(dev.port_idx) + 1) << 16);
    // CErr 3, EP Type Control, and the packet size the descriptor gave.
    let ep0 = (3u32 << 1) | (4u32 << 3) | (u32::from(dev.enumerated.ep0_packet) << 16);
    ctrl.write_ctx32(input_ctx, usize::from(EP0_DCI) + 1, 1, ep0);
    let dequeue = dev.ep0_ring.dequeue();
    ctrl.write_ctx32(input_ctx, usize::from(EP0_DCI) + 1, 2, dequeue as u32);
    ctrl.write_ctx32(input_ctx, usize::from(EP0_DCI) + 1, 3, (dequeue >> 32) as u32);
    ctrl.write_ctx32(input_ctx, usize::from(EP0_DCI) + 1, 4, 8);
    let mut trb = Trb::ZERO;
    trb.param = input_ctx.device_addr();
    trb.control = TRB_ADDRESS_DEVICE | (u32::from(dev.slot_id) << 24);
    trb
}

/// The input context that configures a device's bulk pair, written into the
/// zeroed page at `at`. Every dword is `toyos_xhci::configure`'s: nothing about
/// what the controller is told is decided here.
///
/// **The bind's is the controller's shared page and a rung's is the disk's
/// own.** A rung runs inside a disk call, beside whatever enumeration another
/// port has outstanding, and that enumeration's command may still be reading
/// the shared page.
fn bulk_pair_input_context(
    ctrl: &XhciController,
    at: usize,
    info: &MscInterface,
    in_ring: &TrbRing,
    out_ring: &TrbRing,
    configure: configure::Configure,
) -> Dma<'static> {
    let input_ctx = super::super::zero_dma(ctrl.dma(), at, PAGE);
    let endpoint = |ep: &Endpoint, ring: &TrbRing| BulkEndpoint {
        dci: ep.dci(),
        max_packet: ep.max_packet,
        max_burst: ep.max_burst,
        dequeue: ring.dequeue(),
    };
    let words =
        configure::bulk_pair(endpoint(&info.in_ep, in_ring), endpoint(&info.out_ep, out_ring), configure);
    for word in words {
        ctrl.write_ctx32(input_ctx, word.context, word.dword, word.value);
    }
    input_ctx
}

/// One mass-storage device's pool block and the two bulk rings its endpoint
/// contexts name, as [`prepare`] left them — not rebuilt here, since
/// `TrbRing::init` would zero memory the controller now reads.
#[derive(Clone, Copy)]
pub(in crate::drivers::xhci) struct MscRings {
    /// Which of [`super::super::MSC_BLOCKS`] this is; teardown gives it back.
    at: usize,
    block: usize,
    in_ring: TrbRing,
    out_ring: TrbRing,
    /// The port the device is on, which the ladder resets.
    port_idx: u8,
}

/// Claim a pool block and write its two bulk endpoints into the input
/// context, ready for the Configure Endpoint the sequence issues.
/// Claimed before the endpoints are configured, released only by teardown,
/// so the block is never reissued while the previous holder's contexts
/// still name it. `None` when the pool is out — a refusal, not a failure.
pub(in crate::drivers::xhci) fn prepare(
    ctrl: &mut XhciController,
    slot_id: u8,
    speed: u8,
    port_idx: u8,
    info: &MscInterface,
) -> Option<MscRings> {
    let Some((at, block)) = ctrl.claim_msc_block(port_idx) else {
        log!("usb-storage: slot {slot_id} is the {}th disk; this driver serves {}",
            ctrl.msc_blocks_taken() + 1, super::super::MSC_BLOCKS);
        return None;
    };

    let dma = ctrl.dma();
    let in_ring = TrbRing::init(dma.subview(block + MSC_IN_RING, PAGE));
    let out_ring = TrbRing::init(dma.subview(block + MSC_OUT_RING, PAGE));
    let first = configure::Configure::First { speed, port: port_idx + 1 };
    bulk_pair_input_context(ctrl, OFF_INPUT_CTX, info, &in_ring, &out_ring, first);
    Some(MscRings { at, block, in_ring, out_ring, port_idx })
}

/// Ask the disk what it is and register it if it's one this driver serves.
/// `dev_block` is the device's own block, where its EP0 ring already lives.
/// The last blocking path a scheduler pass can reach: bring-up has to run
/// somewhere and there is no other context that may block.
/// Every failure path already logs; what the answer carries is who keeps the
/// slot, which the caller must act on.
///
/// `described` is the device descriptor's identity and its iSerialNumber.
#[allow(clippy::too_many_arguments)]
pub(in crate::drivers::xhci) fn bind(
    ctrl: &mut XhciController,
    ep0_ring: TrbRing,
    slot_id: u8,
    dev_block: usize,
    rings: MscRings,
    info: &MscInterface,
    enumerated: Enumerated,
    described: (UsbId, u8),
) -> Bind {
    #[cfg(feature = "boot-actuators")]
    if bind_spends_the_scan::take() {
        log!("usb-storage: slot {slot_id} {}", bind_spends_the_scan::WHY);
        let _ = crate::clock::settles(bind_spends_the_scan::SPEND, || false);
        bind_spends_the_scan::hold_answers();
        return Bind::Refused(SlotGoes::Back);
    }
    let MscRings { at, block, in_ring, out_ring, port_idx } = rings;
    let (usb, serial_index) = described;
    let mut dev = MscDevice {
        slot_id,
        pair: *info,
        port_idx,
        enumerated,
        block,
        dev_block,
        ep0_ring,
        in_ring,
        out_ring,
        tag: 0,
        geometry: Geometry::NONE,
        run: Run::NONE,
        failed: false,
        slot_goes: None,
        no_write_cache: false,
        debt: Debt::NONE,
        identity: Identity {
            usb,
            serial: Serial::Absent,
            inquiry: [0; 28],
            sectors: 0,
            sector_bytes: 0,
        },
        reset_at: None,
        left: false,
    };

    #[cfg(feature = "boot-actuators")]
    if let Some((why, stall_ns)) = slow_return::staged().filter(|_| ctrl.awaits_a_device()) {
        log!("usb-storage: slot {slot_id} {why} {} ms before its first command",
            stall_ns / 1_000_000);
        let _ = crate::clock::settles(stall_ns, || false);
    }
    #[cfg(feature = "boot-actuators")]
    let staged = staged::bind_begins();
    let up = bring_up(ctrl, &mut dev);
    #[cfg(feature = "boot-actuators")]
    if staged > 0 {
        log!("usb-storage: slot {slot_id} bound under {staged} staged INQUIRY fault(s): \
             untaken={}", staged::disarm());
    }
    match up {
        Up::Ready => {}
        // A bind that failed without going offline spoke to a transport that
        // answered: its pair has nothing on its rings.
        Up::Refused => return Bind::Refused(dev.slot_goes.unwrap_or(SlotGoes::Back)),
        Up::NotReady => return Bind::NotReady,
    }
    dev.identity.serial = read_serial(ctrl, &mut dev, serial_index);
    log!("usb-storage: slot {slot_id} serial number {}", dev.identity.serial);
    // Machine-wide index: what `usb_storage::handle` looks up by and a mount
    // holds for life, so it must not move when another controller binds or
    // loses a disk — and a device that is a disk this driver's reset lost
    // takes that disk's back.
    if let Some((index, losses, owed)) = ctrl.adopt(&dev.identity, port_idx) {
        dev.debt = Debt::adopted(losses);
        log!("usb-storage: disk {index} came back on port {} slot {slot_id} as the same device \
             (USB {:04x}:{:04x}, serial number {}, {} blocks of {} B), msc_block +{:#x}; its \
             volume carries on{}",
            u32::from(port_idx) + 1, usb.vendor, usb.product, dev.identity.serial, dev.geometry.blocks(),
            dev.geometry.sector_bytes(), block,
            if owed { OWED_A_FLUSH } else { "" });
        ctrl.msc[at].disk = Some(Disk { index, dev });
        return Bind::Bound;
    }
    let index = super::super::DISKS_BOUND.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    log!(
        "usb-storage: disk {index} ready on slot {slot_id}, {} blocks of {} B \
         ({} MiB), msc_block +{:#x}",
        dev.geometry.blocks(),
        dev.geometry.sector_bytes(),
        dev.geometry.blocks() * u64::from(HOST_BLOCK) / (1024 * 1024),
        block
    );
    ctrl.msc[at].disk = Some(Disk { index, dev });
    Bind::Bound
}

/// What the adopting line adds for a disk whose device came back owing a flush.
const OWED_A_FLUSH: &str = ", and it left owing a flush of writes it had reported complete, so \
    the flush of each writer whose writes they were fails";

/// A bind stalled while another disk is held for its device: what it says it
/// is doing, and for how long.
#[cfg(feature = "boot-actuators")]
pub(in crate::drivers::xhci) mod slow_return {
    pub const STALLED: &str = "answers slowly (usb-slow-return): its bind is stalled";

    /// `usb-return-silent`'s: late enough that the operation sent again on a
    /// bound of its own would run past the call's, and early enough that the
    /// held call still sees the device back.
    pub const LATE: &str = "comes back late (usb-return-silent): its bind is stalled";

    pub fn staged() -> Option<(&'static str, u64)> {
        if crate::actuator::usb_slow_return() {
            Some((STALLED, 2_500_000_000))
        } else if crate::actuator::usb_return_silent() {
            Some((LATE, 1_500_000_000))
        } else {
            None
        }
    }
}

/// What came of a bind.
pub(in crate::drivers::xhci) enum Bind {
    /// A disk, which holds the slot.
    Bound,
    /// No disk; the caller releases the claimed block at the unplug, and the
    /// slot goes where this says.
    Refused(SlotGoes),
    /// The device answered and never became ready inside [`READY_BUDGET`]; it
    /// holds nothing on its rings, and its slot goes back.
    NotReady,
}

/// How far [`bring_up`] got.
enum Up {
    /// A disk with a size.
    Ready,
    /// It answered, and never said it was ready: a device a later enumeration
    /// may find ready.
    NotReady,
    /// Anything else, each said by name.
    Refused,
}

/// Everything between a configured interface and a disk with a size, as
/// `toyos_xhci::scsi::BringUp` asks for it.
fn bring_up(ctrl: &mut XhciController, dev: &mut MscDevice) -> Up {
    let slot = dev.slot_id;
    let dma = ctrl.dma();
    let scratch = dma.subview(dev.block + MSC_SCRATCH, MSC_SCRATCH_LEN);
    // No caller budget here: bring-up isn't an operation with a
    // `BlockDevice` handle to answer — it answers only to `READY_BUDGET` and
    // `USB_TIMEOUT_NS`.
    let until = Deadline::never();
    let mut pending = None;
    let mut read = [0u8; MSC_SCRATCH_LEN];
    let (mut up, mut ask) = BringUp::begin(crate::clock::nanos_since_boot() + READY_BUDGET.nanos());
    loop {
        let heard = match ask {
            scsi::Ask::TestUnitReady => match ctrl.bot(dev, &Cdb::TEST_UNIT_READY, None, Asks::Command) {
                Ok(Bot::Done { .. }) => Heard::Good,
                Ok(Bot::Failed) => Heard::CheckCondition,
                Err(why) => {
                    let broke = Told(&why);
                    log!("usb-storage: slot {} broke on TEST UNIT READY: {broke}", dev.slot_id);
                    pending = Some(why);
                    Heard::Broke
                }
            },
            scsi::Ask::RequestSense => Heard::Sense(ctrl.request_sense(dev)),
            scsi::Ask::Recover => {
                let why = pending.take().expect("a recovery is asked for only after a break");
                ctrl.after_break.open(ctrl.bulk_began, AFTER_BREAK);
                // A rung ends on this same command answered, so the run of
                // breaks it counted is over when it says the device is in step.
                if ctrl.climb_until_in_step(dev, why.event(), Left::Elsewhere) {
                    dev.run.over();
                }
                ctrl.after_break = AfterBreak::CLOSED;
                Heard::Recovered { offline: dev.failed }
            }
            scsi::Ask::Read(query) => {
                let (cdb, len) = (query.cdb(), query.allocation());
                scratch.zero();
                // `subview` refuses a command asking for more than the scratch
                // buffer holds. Each command of a bind is a call of its own.
                let reply = ctrl.scsi(dev, &cdb, Some(scratch.subview(0, len)), until);
                ctrl.after_break = AfterBreak::CLOSED;
                match reply {
                    Reply::Ok { delivered } => {
                        dma.copy_to(dev.block + MSC_SCRATCH, &mut read[..len]);
                        Heard::Data { bytes: &read[..len], delivered }
                    }
                    Reply::Refused(sense) => {
                        log_refusal(&cdb, sense);
                        Heard::Unanswered
                    }
                    Reply::Broken | Reply::Budget => Heard::Unanswered,
                }
            }
        };
        let end = match up.heard(heard, crate::clock::nanos_since_boot()) {
            scsi::Next::Ask(next, then) => {
                (up, ask) = (next, then);
                continue;
            }
            scsi::Next::Disk(next, inquiry, then) => {
                log!("usb-storage: slot {slot} vendor {} product {}",
                    Printable(inquiry.vendor()), Printable(inquiry.product()));
                dev.identity.inquiry = inquiry.0;
                (up, ask) = (next, then);
                continue;
            }
            scsi::Next::Up(end) => end,
        };
        return match end {
            scsi::Up::Ready(geometry) => {
                dev.geometry = geometry;
                dev.identity.sectors = geometry.sectors();
                dev.identity.sector_bytes = geometry.sector_bytes();
                Up::Ready
            }
            scsi::Up::Unready { sense, offline } => {
                log!("usb-storage: slot {slot} never became ready, sense {sense}");
                if offline { Up::Refused } else { Up::NotReady }
            }
            scsi::Up::Refused(why) => {
                match why {
                    Refusal::Unanswered(query) => {
                        log!("usb-storage: slot {slot} would not answer {}", query.named());
                    }
                    Refusal::NotADisk(peripheral) => {
                        log!("usb-storage: slot {slot} is SCSI peripheral type {peripheral:#04x}, \
                             not a disk");
                    }
                    Refusal::SectorSize(bytes) => {
                        log!("usb-storage: slot {slot} reports {bytes}-byte blocks; this driver \
                             serves 4096-byte blocks and needs 512..=4096");
                    }
                    Refusal::PastRead10 { last_lba } => {
                        log!("usb-storage: slot {slot} has {} sectors; this driver issues READ(10) \
                             and addresses 2^32", u128::from(last_lba) + 1);
                    }
                    Refusal::LessThanABlock { sectors, sector_bytes } => {
                        log!("usb-storage: slot {slot} holds {sectors} sectors of {sector_bytes} B, \
                             less than one 4096-byte block");
                    }
                }
                Up::Refused
            }
        };
    }
}

/// The serial number string a device's iSerialNumber names (USB 2.0 §9.6.1),
/// in the first language its string descriptor zero offers (§9.6.7): the one
/// field that tells two units of one model apart.
///
/// Into the disk's own data buffer, which nothing else uses before the bind
/// hands the disk over.
fn read_serial(ctrl: &mut XhciController, dev: &mut MscDevice, index: u8) -> Serial {
    if index == 0 {
        return Serial::Absent;
    }
    let dma = ctrl.dma();
    let phys = dma.device_addr() + (dev.block + MSC_DATA) as u64;
    let mut get = |ctrl: &mut XhciController, index: u8, language: u16| -> Option<([u8; 255], usize)> {
        dma.subview(dev.block + MSC_DATA, 255).zero();
        #[cfg(feature = "boot-actuators")]
        let asks = if index != 0 && crate::actuator::usb_serial_short() { 8 } else { 255 };
        #[cfg(not(feature = "boot-actuators"))]
        let asks = 255;
        let asked = ctrl.control_transfer(
            dev.slot_id, dev.dev_block, &mut dev.ep0_ring, 0x80, 0x06,
            0x0300 | u16::from(index), language, Some(phys), asks,
        );
        let Control::Done { delivered } = asked else {
            log!("usb-storage: slot {} would not give string descriptor {index}: {asked}",
                dev.slot_id);
            return None;
        };
        let delivered = usize::from(delivered.min(255));
        let mut arrived = [0u8; 255];
        dma.copy_to(dev.block + MSC_DATA, &mut arrived[..delivered]);
        Some((arrived, delivered))
    };
    let Some((languages, delivered)) = get(ctrl, 0, 0) else { return Serial::Unread };
    let Some(language) = identity::first_language(&languages[..delivered]) else { return Serial::Unread };
    match get(ctrl, index, language) {
        Some((arrived, delivered)) => Serial::from_descriptor(&arrived[..delivered]),
        None => Serial::Unread,
    }
}

/// Read `count` 4 KiB blocks at `lba`. On `Err` the transfer did not happen
/// and `buf` holds nothing the caller may believe.
/// The caller must be inside a block-device operation
/// ([`crate::block::begin_operation`]); a call with no budget established
/// above it is refused by name. [`BlockError::BudgetExpired`] is that
/// refusal; [`BlockError::Device`] is everything else.
///
/// `losses` is set to the disk's loss count as the device that ran the
/// operation counts it (`crate::block::BlockDevice::losses`), here and in
/// [`storage_write`] and [`storage_flush`].
pub fn storage_read(
    index: usize,
    lba: u64,
    count: u32,
    buf: &mut [u8],
    losses: &mut u64,
) -> BlockResult {
    served(index, losses, |ctrl, local| ctrl.msc_read(local, lba, count, buf))
}

pub fn storage_write(
    index: usize,
    lba: u64,
    count: u32,
    buf: &[u8],
    losses: &mut u64,
) -> BlockResult {
    served(index, losses, |ctrl, local| ctrl.msc_write(local, lba, count, buf))
}

pub fn storage_flush(index: usize, losses: &mut u64) -> BlockResult {
    served(index, losses, |ctrl, local| ctrl.msc_flush(local))
}

/// One operation on the machine's `index`-th disk, issued again, whole, on the
/// device that comes back if the disk is held for one.
///
/// **What a filesystem above sees stays true.** A device that left under this
/// driver's reset answered nothing for the command it was inside — no status,
/// so no write in it was ever reported complete — and the reset ended that
/// command at the device (USB 2.0 §9.1.1: every state returns to Default). The
/// command is issued again from the caller's own buffer, whole: every CDB here
/// is idempotent, so blocks of it the device took before it left are written
/// with the same bytes, and a block it took half of is written whole. A device
/// that left owing a flush comes back counting the loss on the disk
/// ([`MscDevice::owes_a_flush`]), read into `losses` under the lock the
/// operation ran under, so the count is the one of the device that ran it.
///
/// **The operation is one call, and its bound is the call's**
/// (`toyos_xhci::call`): opened by the first break or by finding the disk
/// held, carried across every command, the hold and the command sent again,
/// and closed here. The caller spins with `IF` clear for all of it, which is
/// why `CALL_AFTER_BREAK` is held under `time::DEAF_CPU`, the TLB-ack tripwire.
///
/// **The hold only waits for a verdict.** It ends at the disk's own window
/// (`toyos_xhci::identity::RETURN_WINDOW`), past which the disk is lost and the
/// operation fails as any offline disk's does; at the caller's
/// `block::OPERATION`; or where the call's bound says, both of which answer
/// `BudgetExpired`: nothing was issued while it waited, and the caller asks
/// again above every lock.
fn served(
    index: usize,
    losses: &mut u64,
    mut op: impl FnMut(&mut XhciController, usize) -> BlockResult,
) -> BlockResult {
    let until = Operation::deadline();
    let mut call = AfterBreak::CLOSED;
    // Once the disk was found held: until when this call may wait for it, which
    // bounds taking the controller lock again to send the operation too.
    let mut hold_ends = None;
    let mut back = false;
    loop {
        let ran = with_disk_by(index, hold_ends, |ctrl, at| {
            ctrl.after_break = call;
            #[cfg(feature = "boot-actuators")]
            let silent = back && return_silent::begin(index);
            let done = op(ctrl, at);
            #[cfg(feature = "boot-actuators")]
            if silent {
                return_silent::end();
            }
            call = core::mem::replace(&mut ctrl.after_break, AfterBreak::CLOSED);
            if let Some(disk) = &ctrl.msc[at].disk {
                *losses = disk.dev.debt.losses();
            }
            (done, ctrl.msc[at].disk.is_none())
        });
        let (done, held) = match ran {
            Some(Some(ran)) => ran,
            // Not on any controller: held, or gone, which is the wait's to say.
            Some(None) => (Err(BlockError::Device), true),
            None => {
                log!("usb-storage: disk {index} is back, and the controller lock was not free \
                     before this call's bound ended; the operation is asked again");
                return Err(BlockError::BudgetExpired);
            }
        };
        if back {
            log!("usb-storage: disk {index} is back, and the operation it was asked went out \
                 again on it: {}", CameTo(done));
        }
        if !held || done != Err(BlockError::Device) {
            return done;
        }
        let ends = call.hold(crate::clock::nanos_since_boot(), AFTER_BREAK);
        hold_ends = Some(ends);
        match wait_for_return(index, until, ends) {
            Returned::Back => back = true,
            Returned::NotYet => {
                log!("usb-storage: disk {index} {STILL_HELD}");
                return Err(BlockError::BudgetExpired);
            }
            Returned::Lost => return done,
        }
    }
}

/// What a call that ended its hold with the disk still held says.
const STILL_HELD: &str = "is still held when this call may wait no longer; nothing was issued, \
    and the operation is asked again";

/// How a wait for a disk's device ended.
enum Returned {
    /// The disk is served again, by the device that came back.
    Back,
    /// The call may wait no longer; the disk is still waited for.
    NotYet,
    /// The disk is not waited for: it is gone, or not back in time.
    Lost,
}

/// Wait until the disk is back, lost, or `until` or `ends` passes.
///
/// **Only the verdict is waited for here.** Whatever arrives is enumerated and
/// bound where every hot-plugged device is, by the port machine on a CPU that
/// reaches a scheduler pass — never on this one, which spins with `IF` clear —
/// so this asks for the ports to be stepped and looks, without taking the
/// controller lock from a bind that holds it.
fn wait_for_return(index: usize, until: Deadline, ends: u64) -> Returned {
    let mut asked = false;
    loop {
        match look_for(index) {
            Some(Whereabouts::Here) => return Returned::Back,
            Some(Whereabouts::Gone) => return Returned::Lost,
            Some(Whereabouts::Awaited) | None => {}
        }
        if crate::clock::nanos_since_boot() >= ends || until.reached(crate::clock::now()) {
            return Returned::NotYet;
        }
        ports_wanted(!core::mem::replace(&mut asked, true));
        let _ = crate::clock::settles(PORT_LOOK_NS, || false);
    }
}

/// How often a caller waiting for a disk's device looks: under the 100 ms
/// debounce a connect waits out, so the look is never what it waited on.
const PORT_LOOK_NS: u64 = 1_000_000;

/// A block result as one word, for the line that says what a re-issue came to.
struct CameTo(BlockResult);

impl core::fmt::Display for CameTo {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self.0 {
            Ok(()) => "it completed",
            Err(BlockError::BudgetExpired) => "it ran out of its operation budget",
            Err(BlockError::Device) => "it failed",
        })
    }
}

