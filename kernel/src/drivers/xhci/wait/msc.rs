//! USB Mass Storage, Bulk-Only Transport with SCSI (interface class 0x08,
//! subclass 0x06, protocol 0x50) only — no UAS, no CBI/CB, one logical unit,
//! no removable-media handling, no MODE SENSE. One command in flight per
//! controller: `with_disk` holds the controller lock for the whole of it.
//! Everything here comes off the wire and is checked, never trusted; refusal
//! is by name, never a panic.

//! CBW/CSW use [`crate::mm::Unaligned`] (USB BOT §5.1/§5.2 raw bytes) with no
//! concurrent access to race.

use crate::mm::{Dma, Unaligned};

use crate::block::{BlockError, BlockResult};
use crate::log;
use crate::scheduler::Operation;
use crate::time::{Budget, Deadline, Duration};
use super::{Quiet, Restart};
use super::super::{log_unrecoverable, with_disk, Completion, Disk, StorageGeometry, Trb};
use super::super::{TrbRing, XhciController, PAGE, TRB_ADDRESS_DEVICE, TRB_CONFIGURE_EP, TRB_RESET_DEVICE};
use super::super::{stop, CC_SUCCESS, CC_STALL, CC_SHORT_PACKET, TRB_NORMAL, OFF_INPUT_CTX};
use super::super::{AFTER_BREAK, CC_CONTEXT_STATE_ERROR, EP0_DCI};
use super::super::{MSC_IN_RING, MSC_OUT_RING, MSC_CBW, MSC_CSW, MSC_SCRATCH, MSC_SCRATCH_LEN};
use super::super::MSC_INPUT_CTX;
use super::super::{MSC_DATA, MSC_DATA_LEN, MSC_MAX_BLOCKS, MSC_STRIDE};
use super::super::device::Endpoint;
use toyos_xhci::bot::Phase;
use toyos_xhci::call::AfterBreak;
use toyos_xhci::configure::{self, BulkEndpoint};
use toyos_xhci::ladder::{self, AfterReset, Left, PortStep, Rung};
use toyos_xhci::port;
use toyos_xhci::reset_recovery::{self, Answered, GaveUp, Look, Pipe, Quiescing, SlotGoes, Step};

/// A region, not an address: the CBW's length is the region's own size, so
/// no command can name a length its destination lacks.
type DataPhase = Option<Dma<'static>>;


/// The block size everything above this driver is written in; a device
/// whose sizes don't divide it is unimplemented, not approximated.
const HOST_BLOCK: u32 = 4096;

/// Wall-clock budget on bring-up's ready attempts: bounds when [`bring_up`]
/// stops *starting* attempts, not the one already running.
const READY_BUDGET: Budget = Budget::of(
    Duration::from_millis(500),
    "the device is reported as not becoming ready and the boot goes on without it",
);

/// The most breaks a device's transport gets in a row before the device is
/// offline: one per rung of `toyos_xhci::ladder`.
///
/// **Per device, not per command**: the run it bounds is the device's, however
/// many callers and operations it is spread over, and only a completed round
/// trip ends it.
pub(in crate::drivers::xhci) const MAX_TRANSPORT_BREAKS: u8 = ladder::MOST_BREAKS;

const CBW_SIGNATURE: u32 = 0x4342_5355;
const CSW_SIGNATURE: u32 = 0x5342_5355;
const CBW_LEN: u32 = 31;
const CSW_LEN: u32 = 13;
const TEST_UNIT_READY: [u8; 6] = [0x00; 6];

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
    logical_block_bytes: u32,
    sectors_per_block: u32,
    blocks: u64,
    /// Transport breaks in a row, each answered with one rung of the ladder,
    /// and the highest rung among them; a completed round trip clears both.
    breaks: u8,
    climbed: Option<Rung>,
    /// Set once the device was taken offline; the device is not spoken to again.
    failed: bool,
    /// Where [`XhciController::take_offline`] said the slot goes, until
    /// somebody has acted on it; see [`Self::take_slot_owed`].
    slot_goes: Option<SlotGoes>,
    /// Set once the device refuses SYNCHRONIZE CACHE; logged once, not per
    /// flush — a log line would itself be pending content the next flush drains.
    no_write_cache: bool,
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

    pub fn geometry(&self) -> StorageGeometry {
        StorageGeometry {
            logical_block_bytes: self.logical_block_bytes,
            blocks: self.blocks,
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

/// How one Bulk-Only round trip ended; `delivered` never exceeds the
/// transfer it describes.
enum Bot {
    /// CSW status 0; `delivered` is the smaller of what the controller moved
    /// and the device says it didn't — else stale data from an earlier LBA leaks.
    Done { delivered: u32 },
    /// CSW status 1: the device understood and refused. Sense data says why.
    Failed,
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
    pub ep0_packet: u16,
    pub configuration: u8,
}

/// Why a Bulk-Only round trip could not be completed; what happened decides
/// which recovery command is legal.
enum Broke {
    /// The controller reported this completion code for the named phase, on
    /// this pipe.
    Code { phase: &'static str, code: u32, pipe: Pipe },
    /// Nothing came back for the named phase, for [`Quiet`]'s reason.
    Silence { phase: &'static str, why: Quiet },
    /// The phase moved the wrong byte count; CBW/CSW are fixed length, so
    /// short is not a short transfer.
    Short { phase: &'static str, moved: u32, wanted: u32 },
    /// The endpoint stalled and the reset did not take.
    Stall { phase: &'static str },
    /// CSW status 2: a phase error, which leaves both endpoints Running, so
    /// an unconditional Reset Endpoint is illegal here.
    PhaseError,
    /// The CSW arrived and named somebody else's transfer. The status and
    /// residue are the rest of what the device said, and tell a status the
    /// device made for an abandoned command from one it made for nothing.
    Csw { what: &'static str, got: u32, want: u32, status: u8, residue: u32 },
    /// More bytes claimed unmoved than the transfer had; believing it would
    /// underflow the byte count every caller uses.
    Residue { unmoved: u32, of: u32 },
}

impl Broke {
    /// Where the break left the device. A data phase that did not complete,
    /// of a command whose data goes out, is a device owed bytes; `data_in` is
    /// the direction of the command that broke.
    fn left(&self, data_in: bool) -> Left {
        let in_data = match self {
            Self::Code { phase, .. } | Self::Silence { phase, .. } | Self::Stall { phase } => {
                *phase == Phase::Data.named()
            }
            Self::Short { .. } | Self::PhaseError | Self::Csw { .. } | Self::Residue { .. } => false,
        };
        if in_data && !data_in {
            Left::OwedDataOut
        } else {
            Left::Elsewhere
        }
    }

    /// The transfer event that ended the round trip, where one did: what the
    /// quiesce takes ahead of the pipe's Endpoint State field.
    fn event(&self) -> Option<(Pipe, u32)> {
        match self {
            Self::Code { code, pipe, .. } => Some((*pipe, *code)),
            _ => None,
        }
    }
}

impl core::fmt::Display for Broke {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Code { phase, code, .. } => {
                write!(f, "{phase} phase completion {}", Completion(*code))
            }
            Self::Silence { phase, why } => why.about(phase, "phase", f),
            Self::Short { phase, moved, wanted } => {
                write!(f, "{phase} phase moved {moved} of {wanted} B")
            }
            Self::Stall { phase } => {
                write!(f, "the {phase} phase stalled and the endpoint reset did not clear it")
            }
            Self::PhaseError => f.write_str("the device reported a phase error"),
            Self::Csw { what, got, want, status, residue } => write!(
                f,
                "CSW {what} {got:#x}, not {want:#x} (status {status}, {residue} B unmoved)"
            ),
            Self::Residue { unmoved, of } => {
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

    /// Called where the driver is about to run a WRITE(10) data phase.
    pub fn arm() {
        if !crate::actuator::usb_transport_break() {
            return;
        }
        ARMED.store(UNSPENT.swap(false, Ordering::Relaxed), Ordering::Relaxed);
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
        LEFT.store(n, Ordering::Relaxed);
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
        let only = ONLY.load(Ordering::Relaxed);
        if only != ANY && only != u16::from(opcode) {
            return None;
        }
        let mut left = LEFT.load(Ordering::Relaxed);
        loop {
            let less = left.checked_sub(1)?;
            match LEFT.compare_exchange_weak(left, less, Ordering::Relaxed, Ordering::Relaxed) {
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

/// The completion of one SCSI command, after the transport's own recovery.
enum Scsi {
    Ok { delivered: u32 },
    /// Understood and declined, carrying the sense key/ASC/ASCQ: an optional
    /// command's caller must tell "I will not" from "I cannot".
    Refused { key: u8, asc: u8, ascq: u8 },
    /// The transport broke, or the device contradicted itself; nothing about
    /// the buffer is known.
    Broken,
    /// Not issued: the caller's [`crate::block::OPERATION`] budget had
    /// already expired. Distinct from [`Self::Broken`] because it is not a
    /// fact about the disk — [`MscDevice::failed`] stays clear.
    Budget,
}

impl Scsi {
    /// SBC's ILLEGAL REQUEST/INVALID COMMAND OPERATION CODE: an answer, not
    /// a failure, for a command SBC makes optional.
    fn unimplemented(&self) -> bool {
        matches!(self, Self::Refused { key: 0x05, asc: 0x20, ascq: 0x00 })
    }

    /// `Ok` never reaches here — each of the three callers has its own idea
    /// of what a complete transfer is.
    fn as_block_error(&self) -> BlockError {
        match self {
            Self::Budget => BlockError::BudgetExpired,
            _ => BlockError::Device,
        }
    }
}

/// The one line a device's refusal produces, wherever it is noticed — one
/// function so per-caller wording never obscures what the device said.
fn log_refusal(cdb: &[u8], key: u8, asc: u8, ascq: u8) {
    log!(
        "usb-storage: SCSI {:#04x} failed, sense {key:#04x}/{asc:#04x}/{ascq:#04x}",
        cdb.first().copied().unwrap_or(0)
    );
}

/// The sense a test actuator makes SYNCHRONIZE CACHE answer with, or `None`
/// on a shipped kernel. ILLEGAL REQUEST/INVALID COMMAND OPERATION CODE must
/// not fail the caller; HARDWARE ERROR/INTERNAL TARGET FAILURE must.
fn flush_sense() -> Option<(u8, u8, u8)> {
    if crate::actuator::usb_flush_unimplemented() {
        Some((0x05, 0x20, 0x00))
    } else if crate::actuator::usb_flush_fails() {
        Some((0x04, 0x44, 0x00))
    } else {
        None
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
        crate::arch::cpu::read_rsp(),
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
            // LBA 0, count 0: the whole medium — all a block-device flush can mean.
            let cdb = [0x35u8, 0, 0, 0, 0, 0, 0, 0, 0, 0];
            let issued = ctrl.scsi(dev, &cdb, 10, None, false, until);
            let outcome = match flush_sense() {
                Some((key, asc, ascq)) => Scsi::Refused { key, asc, ascq },
                None => issued,
            };
            // No write cache means nothing here could have been made durable,
            // so reporting a failure would report the wrong thing.
            if outcome.unimplemented() {
                if !dev.no_write_cache {
                    dev.no_write_cache = true;
                    log!("usb-storage: disk {number} does not implement SYNCHRONIZE CACHE \
                         (sense 0x05/0x20/0x00); its writes are durable once they complete");
                }
                return Ok(());
            }
            match outcome {
                Scsi::Ok { .. } => Ok(()),
                Scsi::Refused { key, asc, ascq } => {
                    log_refusal(&cdb, key, asc, ascq);
                    Err(BlockError::Device)
                }
                Scsi::Broken => Err(BlockError::Device),
                // Unlogged: `scsi` already named the budget, and a line here
                // would itself be the next flush.
                Scsi::Budget => Err(BlockError::BudgetExpired),
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
        if count == 0 {
            return Ok(());
        }
        match lba.checked_add(count as u64) {
            Some(end) if end <= dev.blocks => {}
            _ => {
                log!("usb-storage: {lba}+{count} is past the {} blocks this disk has", dev.blocks);
                return Err(BlockError::Device);
            }
        }

        let dma = self.dma();
        let data = dma.subview(dev.block + MSC_DATA, MSC_DATA_LEN);
        let mut done = 0u32;
        while done < count {
            let batch = (count - done).min(MSC_MAX_BLOCKS);
            let bytes = batch as usize * HOST_BLOCK as usize;
            let offset = done as usize * HOST_BLOCK as usize;
            let sector_lba = (lba + done as u64) * dev.sectors_per_block as u64;
            let sectors = batch * dev.sectors_per_block;

            // `bring_up` refused any disk whose last sector doesn't fit 32
            // bits, so READ/WRITE(10) can address every block reported.
            let lba32 = sector_lba as u32;
            let cdb = [
                if write { 0x2Au8 } else { 0x28 },
                0,
                (lba32 >> 24) as u8,
                (lba32 >> 16) as u8,
                (lba32 >> 8) as u8,
                lba32 as u8,
                0,
                (sectors >> 8) as u8,
                sectors as u8,
                0,
            ];

            if let Host::From(src) = &host {
                dma.copy_from(dev.block + MSC_DATA, &src[offset..offset + bytes]);
            }

            match self.scsi(dev, &cdb, 10, Some(data.subview(0, bytes)), !write, until) {
                Scsi::Ok { delivered } if delivered as usize == bytes => {}
                // Short of what was asked: nothing above can say which
                // blocks arrived, so a partial transfer is a failed one.
                Scsi::Ok { delivered } => {
                    log!("usb-storage: {delivered} of {bytes} B at block {}", lba + done as u64);
                    return Err(BlockError::Device);
                }
                Scsi::Refused { key, asc, ascq } => {
                    log_refusal(&cdb, key, asc, ascq);
                    return Err(BlockError::Device);
                }
                // `done > 0` means blocks already moved are on the device
                // with no way to resume; only the first batch may answer "ask
                // again".
                other @ (Scsi::Broken | Scsi::Budget) => {
                    return Err(if done == 0 { other.as_block_error() } else { BlockError::Device });
                }
            }

            if let Host::Into(dst) = &mut host {
                dma.copy_to(dev.block + MSC_DATA, &mut dst[offset..offset + bytes]);
            }
            done += batch;
        }
        Ok(())
    }

    /// One SCSI command with transport recovery applied and re-issued;
    /// every CDB here is idempotent, so a re-issue is genuine. `until` is
    /// checked only here, between commands — it costs the device nothing (no
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
    /// rung by rung** ([`AFTER_BREAK`]): opened by the first break, closed here
    /// on every way out, so no call inherits another's.
    #[allow(clippy::too_many_arguments)]
    fn scsi(
        &mut self,
        dev: &mut MscDevice,
        cdb: &[u8],
        cdb_len: u8,
        data: DataPhase,
        data_in: bool,
        until: Deadline,
    ) -> Scsi {
        let answer = self.scsi_within_one_budget(dev, cdb, cdb_len, data, data_in, until);
        self.after_break = AfterBreak::CLOSED;
        answer
    }

    #[allow(clippy::too_many_arguments)]
    fn scsi_within_one_budget(
        &mut self,
        dev: &mut MscDevice,
        cdb: &[u8],
        cdb_len: u8,
        data: DataPhase,
        data_in: bool,
        until: Deadline,
    ) -> Scsi {
        let opcode = cdb.first().copied().unwrap_or(0);
        // Named per line so a multi-disk boot's retry log attributes to the
        // right disk.
        let slot = self.slot(dev.slot_id);
        loop {
            if until.reached(crate::clock::now()) {
                log!("usb-storage: {slot} SCSI {opcode:#04x} not issued: {}",
                    crate::block::OPERATION);
                // Not `Broken`: nothing was issued and `dev.failed` stays
                // untouched.
                return Scsi::Budget;
            }
            // A command re-issued with nothing left would have every wait cut
            // at once, and count against the device a break that was the
            // budget's.
            if let Err(why) = self.after_break.request(crate::clock::nanos_since_boot()) {
                log!("usb-storage: {slot} SCSI {opcode:#04x} not issued again: {why}");
                return Scsi::Budget;
            }
            match self.bot(dev, cdb, cdb_len, data, data_in, Asks::Command) {
                Ok(Bot::Done { delivered }) => {
                    self.transport_came_back(dev, opcode);
                    return Scsi::Ok { delivered };
                }
                Ok(Bot::Failed) => {
                    self.transport_came_back(dev, opcode);
                    let (key, asc, ascq) = self.request_sense(dev);
                    return Scsi::Refused { key, asc, ascq };
                }
                // Not a transport that broke: a device that is no longer on
                // the bus. Its port's own teardown gives the slot and the pool
                // block back, and a recovery or a reset aimed at an empty port
                // would only spend their bounds.
                Err(broke @ Broke::Silence { why: Quiet::Gone, .. }) => {
                    log!("usb-storage: {slot} transport broke on SCSI {opcode:#04x}: {broke}; \
                         its port's teardown takes it from here");
                    dev.failed = true;
                    return Scsi::Broken;
                }
                Err(broke) => {
                    self.after_break.open(self.bulk_began, AFTER_BREAK);
                    log!("usb-storage: {slot} transport broke on SCSI {opcode:#04x}: {broke}; \
                         break {} of {MAX_TRANSPORT_BREAKS} running", dev.breaks.saturating_add(1));
                    if !self.climb_until_in_step(dev, broke.event(), broke.left(data_in)) {
                        return Scsi::Broken;
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
            dev.breaks = dev.breaks.saturating_add(1);
            let rung = ladder::next(dev.climbed, left);
            if dev.climbed.is_none() && rung != Rung::ClassReset {
                log!("usb-storage: {slot} is owed the data of the command that broke, so nothing \
                     can be asked of it on the Bulk-Out: its port is reset with no class reset \
                     before it");
            }
            dev.climbed = Some(rung);
            self.after_break.enter(rung, crate::clock::nanos_since_boot());
            let climbed = match rung {
                Rung::ClassReset => self.reset_recovery(dev, broke),
                Rung::PortReset => self.port_reset_recovery(dev, broke),
                Rung::Offline => {
                    log!("usb-storage: {slot} broke {} times running; its port reset did not \
                         bring the transport back", dev.breaks);
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
                    log!("usb-storage: {slot} transport broke on the {}'s TEST UNIT READY: {why}; \
                         break {} of {MAX_TRANSPORT_BREAKS} running",
                        rung.named(), dev.breaks.saturating_add(1));
                    broke = why.event();
                }
                // As a round trip whose port read disconnected mid-wait: a
                // reset aimed at an empty port would only spend its bound.
                Climbed::Gone => {
                    dev.failed = true;
                    return false;
                }
                Climbed::Failed => {
                    log!("usb-storage: {slot} the {} was not answered; break {} of \
                         {MAX_TRANSPORT_BREAKS} running", rung.named(), dev.breaks.saturating_add(1));
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
        let breaks = core::mem::take(&mut dev.breaks);
        dev.climbed = None;
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
    fn reset_port(&mut self, dev: &MscDevice, why: &str) -> AfterReset {
        let port_idx = dev.port_idx;
        let before = self.read_portsc(port_idx);
        let protocol = self.protocols.of(port_idx);
        let kind = port::offline_reset(protocol);
        // A port that reads empty, or connected across a gap, no longer holds
        // the device this is about: its teardown owns what is left.
        let here = before.connected() && !before.connect_changed();
        let finished = here && {
            self.write_portsc(port_idx, port::reset_write(kind, before).acknowledging_reset(before));
            self.settles_within_call(|| self.read_portsc(port_idx).reset_changed())
        };
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
                PortStep::Reset => match self.reset_port(dev, "recovering") {
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
        match self.bot(dev, &TEST_UNIT_READY, 6, None, false, Asks::Verification(Rung::PortReset)) {
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
            let (key, asc, ascq) = self.request_sense(dev);
            log!("usb-storage: {} held sense {key:#04x}/{asc:#04x}/{ascq:#04x} after its recovery",
                self.slot(dev.slot_id));
        }
    }

    /// REQUEST SENSE as (key, ASC, ASCQ), zeroed if the device would not
    /// say — zero is the failing side of every decision made from it.
    fn request_sense(&mut self, dev: &mut MscDevice) -> (u8, u8, u8) {
        let dma = self.dma();
        let scratch = dma.subview(dev.block + MSC_SCRATCH, MSC_SCRATCH_LEN);
        scratch.zero();
        let cdb = [0x03u8, 0, 0, 0, 18, 0];
        // Goes through `bot` directly, so it cannot recurse into asking for
        // sense about itself. ASCQ is byte 13, so 14 bytes must arrive or all
        // three stay zero, which is what `Scsi::unimplemented` tests for.
        match self.bot(dev, &cdb, 6, Some(scratch.subview(0, 18)), true, Asks::Command) {
            Ok(Bot::Done { delivered }) if delivered >= 14 => {
                let mut resp = [0u8; 18];
                dma.copy_to(dev.block + MSC_SCRATCH, &mut resp);
                (resp[2] & 0x0F, resp[12], resp[13])
            }
            _ => (0, 0, 0),
        }
    }

    /// One fixed-length leg of the round trip (command or status block),
    /// which the device must take or give in full.
    fn framed_phase(
        &mut self,
        dev: &mut MscDevice,
        in_dir: bool,
        phys: u64,
        len: u32,
        phase: Phase,
        open: &stop::OpenCommand,
    ) -> Result<(), Broke> {
        let what = phase.named();
        match self.bulk(dev, in_dir, phys, len, phase, open) {
            // Short Packet is how the xHC reports a sub-maximum-packet
            // transfer (a 13-byte CSW on a 512-byte endpoint); zero residue
            // means it all arrived.
            Ok((CC_SUCCESS | CC_SHORT_PACKET, 0)) => Ok(()),
            Ok((CC_SUCCESS | CC_SHORT_PACKET, residue)) => Err(Broke::Short {
                phase: what,
                moved: len.saturating_sub(residue),
                wanted: len,
            }),
            Ok((code, _)) => Err(Broke::Code { phase: what, code, pipe: Pipe::of(in_dir) }),
            Err(why) => Err(Broke::Silence { phase: what, why }),
        }
    }

    /// The Bulk-Only Transport round trip: command block out, data, status in.
    fn bot(
        &mut self,
        dev: &mut MscDevice,
        cdb: &[u8],
        cdb_len: u8,
        data: DataPhase,
        data_in: bool,
        asks: Asks,
    ) -> Result<Bot, Broke> {
        // The CDBs are this file's own, so their shape is a kernel invariant.
        assert!(cdb_len as usize <= cdb.len() && cdb_len <= 16);
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
            Asks::Command => staged::take(cdb.first().copied().unwrap_or(0)),
        };
        #[cfg(not(feature = "boot-actuators"))]
        let _ = asks;
        #[cfg(feature = "boot-actuators")]
        if staged == Some(staged::Fault::PortGone) {
            return Err(Broke::Silence { phase: "command", why: Quiet::Gone });
        }
        #[cfg(feature = "boot-actuators")]
        if staged == Some(staged::Fault::Unanswered) {
            let began = crate::clock::nanos_since_boot();
            let _ = self.settles_within_call(|| false);
            let now = crate::clock::nanos_since_boot();
            let cut = self.after_break.cut(began, now, super::super::USB_TIMEOUT_NS);
            let why = if cut { Quiet::Spent } else { Quiet::Elapsed };
            return Err(Broke::Silence { phase: "status", why });
        }

        let dma = self.dma();
        let tag = dev.next_tag();
        #[cfg(feature = "stack-witness")]
        let entered_with = block_witness(dev);
        // Unaligned per the file header; bounded by CBW_LEN (15+cdb_len <=
        // 31) and exclusive — not yet enqueued.
        let cbw: Dma<'static, Unaligned> =
            super::super::zero_dma(dma, dev.block + MSC_CBW, CBW_LEN as usize).unaligned();
        cbw.write::<u32>(0, CBW_SIGNATURE.to_le());
        cbw.write::<u32>(4, tag.to_le());
        cbw.write::<u32>(8, data_len.to_le());
        cbw.write::<u8>(12, if data_in { 0x80 } else { 0x00 });
        cbw.write::<u8>(13, 0); // LUN 0: this driver binds one logical unit
        cbw.write::<u8>(14, cdb_len);
        cbw.copy_from(15, &cdb[..cdb_len as usize]);
        #[cfg(feature = "boot-actuators")]
        if staged == Some(staged::Fault::BadSignature) {
            cbw.write::<u32>(0, 0);
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

        let cbw_phys = dma.device_addr() + (dev.block + MSC_CBW) as u64;
        #[cfg(feature = "boot-actuators")]
        let withheld = staged == Some(staged::Fault::NoCbw);
        #[cfg(not(feature = "boot-actuators"))]
        let withheld = false;
        if !withheld {
            self.framed_phase(dev, false, cbw_phys, CBW_LEN, Phase::Command, &open)?;
        }

        // What the controller says reached the buffer; checked against the
        // CSW's residue below.
        let mut moved = 0u32;
        if data_len > 0 {
            // The gap the class leaves open: the device has the CBW and this
            // kernel has queued nothing for it.
            open.at(Phase::DataOwed, &dev.in_ring, &dev.out_ring);
            #[cfg(feature = "boot-actuators")]
            if cdb.first() == Some(&0x2A) {
                transport_break::arm();
                mid_write::wedge_if_staged(Phase::DataOwed);
            }
            #[cfg(feature = "boot-actuators")]
            let held = short_read::hold(
                dma,
                (data_phys - dma.device_addr()) as usize,
                data_len,
                data_in && cdb.first() == Some(&0x28),
            );
            let completion = self.bulk(dev, data_in, data_phys, data_len, Phase::Data, &open);
            #[cfg(feature = "boot-actuators")]
            let completion = short_read::release(dma, held, completion);
            match completion {
                Ok((CC_SUCCESS | CC_SHORT_PACKET, unmoved)) => {
                    moved = data_len.saturating_sub(unmoved);
                }
                // A stalled data phase is ordinary (unsupported command,
                // read past the end); recovering it and reading the status
                // turns it into a clean refusal.
                Ok((CC_STALL, unmoved)) => {
                    if !self.restart_bulk(dev, data_in) {
                        return Err(Broke::Stall { phase: "data" });
                    }
                    // The recovery rebuilt the ring this phase was on, so the
                    // point the account reads is republished before anything
                    // else reaches the controller.
                    open.at(Phase::Data, &dev.in_ring, &dev.out_ring);
                    moved = data_len.saturating_sub(unmoved);
                }
                Ok((code, _)) => {
                    return Err(Broke::Code { phase: "data", code, pipe: Pipe::of(data_in) })
                }
                Err(why) => return Err(Broke::Silence { phase: "data", why }),
            }
        }

        // The second gap: the data phase is done, or there was none, and the
        // device is holding a CSW nothing has asked for.
        open.at(Phase::StatusOwed, &dev.in_ring, &dev.out_ring);
        #[cfg(feature = "boot-actuators")]
        if data_len > 0 && cdb.first() == Some(&0x2A) {
            mid_write::wedge_if_staged(Phase::StatusOwed);
        }
        let csw_phys = dma.device_addr() + (dev.block + MSC_CSW) as u64;
        super::super::zero_dma(dma, dev.block + MSC_CSW, CSW_LEN as usize);
        let mut got = self.framed_phase(dev, true, csw_phys, CSW_LEN, Phase::Status, &open);
        if let Err(Broke::Code { code: CC_STALL, .. }) = got {
            // The spec's one legal retry: the device may stall the status
            // phase once.
            if !self.restart_bulk(dev, true) {
                return Err(Broke::Stall { phase: "status" });
            }
            open.at(Phase::StatusOwed, &dev.in_ring, &dev.out_ring);
            super::super::zero_dma(dma, dev.block + MSC_CSW, CSW_LEN as usize);
            got = self.framed_phase(dev, true, csw_phys, CSW_LEN, Phase::Status, &open);
        }
        got?;

        #[cfg(feature = "stack-witness")]
        block_witness_holds(dev, entered_with);
        // Unaligned again; bounded by the CSW_LEN subview, exclusive because
        // `framed_phase` returned `Ok`. Every field is checked below, never
        // believed.
        let csw = dma.subview(dev.block + MSC_CSW, CSW_LEN as usize).unaligned();
        let (signature, csw_tag, residue, status) = (
            u32::from_le(csw.read::<u32>(0)),
            u32::from_le(csw.read::<u32>(4)),
            u32::from_le(csw.read::<u32>(8)),
            csw.read::<u8>(12),
        );
        if signature != CSW_SIGNATURE {
            return Err(Broke::Csw {
                what: "signature",
                got: signature,
                want: CSW_SIGNATURE,
                status,
                residue,
            });
        }
        // Accepting a mismatched tag would attribute one command's status
        // to another — a write reporting the read before it as success.
        if csw_tag != tag {
            return Err(Broke::Csw { what: "tag", got: csw_tag, want: tag, status, residue });
        }
        if residue > data_len {
            return Err(Broke::Residue { unmoved: residue, of: data_len });
        }
        match status {
            // Neither account is trusted alone: a caller may read only what
            // both the device and the controller say arrived.
            0 => Ok(Bot::Done { delivered: moved.min(data_len - residue) }),
            1 => Ok(Bot::Failed),
            _ => Err(Broke::PhaseError),
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
        match self.bot(dev, &TEST_UNIT_READY, 6, None, false, Asks::Verification(Rung::ClassReset)) {
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
pub(in crate::drivers::xhci) fn bind(
    ctrl: &mut XhciController,
    ep0_ring: TrbRing,
    slot_id: u8,
    dev_block: usize,
    rings: MscRings,
    info: &MscInterface,
    enumerated: Enumerated,
) -> Bind {
    let MscRings { at, block, in_ring, out_ring, port_idx } = rings;
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
        logical_block_bytes: 0,
        sectors_per_block: 0,
        blocks: 0,
        breaks: 0,
        climbed: None,
        failed: false,
        slot_goes: None,
        no_write_cache: false,
    };

    #[cfg(feature = "boot-actuators")]
    let staged = staged::bind_begins();
    let up = bring_up(ctrl, &mut dev);
    #[cfg(feature = "boot-actuators")]
    if staged > 0 {
        log!("usb-storage: slot {slot_id} bound under {staged} staged INQUIRY fault(s): \
             untaken={}", staged::disarm());
    }
    if !up {
        // A bind that failed without going offline spoke to a transport that
        // answered: its pair has nothing on its rings.
        return Bind::Refused(dev.slot_goes.unwrap_or(SlotGoes::Back));
    }
    // Machine-wide index: what `usb_storage::handle` looks up by and a mount
    // holds for life, so it must not move when another controller binds or
    // loses a disk.
    let index = super::super::DISKS_BOUND.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    log!(
        "usb-storage: disk {index} ready on slot {slot_id}, {} blocks of {} B \
         ({} MiB), msc_block +{:#x}",
        dev.blocks,
        dev.logical_block_bytes,
        dev.blocks * HOST_BLOCK as u64 / (1024 * 1024),
        block
    );
    ctrl.msc[at].disk = Some(Disk { index, dev });
    Bind::Bound
}

/// What came of a bind.
pub(in crate::drivers::xhci) enum Bind {
    /// A disk, which holds the slot.
    Bound,
    /// No disk; the caller releases the claimed block at the unplug, and the
    /// slot goes where this says.
    Refused(SlotGoes),
}

/// TEST UNIT READY, INQUIRY and READ CAPACITY: everything between a configured
/// interface and a disk with a size.
fn bring_up(ctrl: &mut XhciController, dev: &mut MscDevice) -> bool {
    // Drives the transport directly, not `scsi`: NOT READY is expected, not
    // an error, so it must not log per attempt, and fetching sense also
    // clears the condition on a device still spinning up.
    let give_up = crate::clock::nanos_since_boot() + READY_BUDGET.nanos();
    let mut sense = (0u8, 0u8, 0u8);
    let mut ready = false;
    loop {
        match ctrl.bot(dev, &TEST_UNIT_READY, 6, None, false, Asks::Command) {
            Ok(Bot::Done { .. }) => {
                ready = true;
                break;
            }
            Ok(Bot::Failed) => sense = ctrl.request_sense(dev),
            Err(broke) => {
                log!("usb-storage: slot {} broke on TEST UNIT READY: {broke}", dev.slot_id);
                ctrl.after_break.open(ctrl.bulk_began, AFTER_BREAK);
                // A rung ends on this same command answered, so the run of
                // breaks it counted is over when it says the device is in step.
                if ctrl.climb_until_in_step(dev, broke.event(), Left::Elsewhere) {
                    dev.breaks = 0;
                    dev.climbed = None;
                }
                ctrl.after_break = AfterBreak::CLOSED;
            }
        }
        if dev.failed || crate::clock::nanos_since_boot() >= give_up {
            break;
        }
    }
    if !ready {
        log!("usb-storage: slot {} never became ready, sense {:#04x}/{:#04x}/{:#04x}",
            dev.slot_id, sense.0, sense.1, sense.2);
        return false;
    }

    let dma = ctrl.dma();
    let scratch = dma.subview(dev.block + MSC_SCRATCH, MSC_SCRATCH_LEN);
    // No caller budget here: bring-up isn't an operation with a
    // `BlockDevice` handle to answer — it answers only to `READY_BUDGET` and
    // `USB_TIMEOUT_NS`.
    let until = Deadline::never();
    let read_scratch = |ctrl: &mut XhciController,
                        dev: &mut MscDevice,
                        cdb: &[u8],
                        cdb_len: u8,
                        want: u32,
                        out: &mut [u8]| {
        scratch.zero();
        // `subview` refuses a command asking for more than the scratch
        // buffer holds.
        match ctrl.scsi(dev, cdb, cdb_len, Some(scratch.subview(0, want as usize)), true, until) {
            Scsi::Ok { delivered } if delivered as usize >= out.len() => {
                dma.copy_to(dev.block + MSC_SCRATCH, out);
                true
            }
            Scsi::Refused { key, asc, ascq } => {
                log_refusal(cdb, key, asc, ascq);
                false
            }
            _ => false,
        }
    };

    let mut inquiry = [0u8; 36];
    if !read_scratch(ctrl, dev, &[0x12u8, 0, 0, 0, 36, 0], 6, 36, &mut inquiry) {
        log!("usb-storage: slot {} would not answer INQUIRY", dev.slot_id);
        return false;
    }
    let peripheral = inquiry[0] & 0x1F;
    if peripheral != 0 {
        log!("usb-storage: slot {} is SCSI peripheral type {peripheral:#04x}, not a disk",
            dev.slot_id);
        return false;
    }
    log!("usb-storage: slot {} vendor {} product {}", dev.slot_id,
        Printable(&inquiry[8..16]), Printable(&inquiry[16..32]));

    // READ CAPACITY(10) reports an all-ones last LBA when the disk needs the
    // 16-byte form to describe its size.
    let mut cap10 = [0u8; 8];
    if !read_scratch(ctrl, dev, &[0x25u8, 0, 0, 0, 0, 0, 0, 0, 0, 0], 10, 8, &mut cap10) {
        log!("usb-storage: slot {} would not answer READ CAPACITY(10)", dev.slot_id);
        return false;
    }
    let (last_lba, block_bytes) = if u32::from_be_bytes([cap10[0], cap10[1], cap10[2], cap10[3]])
        == u32::MAX
    {
        let mut cap16 = [0u8; 12];
        let cdb = [0x9Eu8, 0x10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 32, 0, 0];
        if !read_scratch(ctrl, dev, &cdb, 16, 32, &mut cap16) {
            log!("usb-storage: slot {} would not answer READ CAPACITY(16)", dev.slot_id);
            return false;
        }
        (
            u64::from_be_bytes([
                cap16[0], cap16[1], cap16[2], cap16[3], cap16[4], cap16[5], cap16[6], cap16[7],
            ]),
            u32::from_be_bytes([cap16[8], cap16[9], cap16[10], cap16[11]]),
        )
    } else {
        (
            u32::from_be_bytes([cap10[0], cap10[1], cap10[2], cap10[3]]) as u64,
            u32::from_be_bytes([cap10[4], cap10[5], cap10[6], cap10[7]]),
        )
    };

    // A zero or >4096 block size divides by zero below (`4096 /
    // block_bytes`); the allowed set is which sizes divide the 4 KiB host
    // block.
    if !matches!(block_bytes, 512 | 1024 | 2048 | 4096) {
        log!("usb-storage: slot {} reports {block_bytes}-byte blocks; this driver \
             serves 4096-byte blocks and needs 512..=4096", dev.slot_id);
        return false;
    }
    // READ/WRITE(10) carry a 32-bit LBA; serving the first 2 TiB of a
    // bigger disk would silently truncate it.
    if last_lba > u32::MAX as u64 {
        log!("usb-storage: slot {} has {} sectors; this driver issues READ(10) and \
             addresses 2^32", dev.slot_id, last_lba as u128 + 1);
        return false;
    }
    let sectors = last_lba + 1;
    let sectors_per_block = HOST_BLOCK / block_bytes;
    let blocks = sectors / sectors_per_block as u64;
    if blocks == 0 {
        log!("usb-storage: slot {} holds {sectors} sectors of {block_bytes} B, less \
             than one 4096-byte block", dev.slot_id);
        return false;
    }

    dev.logical_block_bytes = block_bytes;
    dev.sectors_per_block = sectors_per_block;
    dev.blocks = blocks;
    true
}

/// A device-supplied ASCII field, rendered without letting it choose what the
/// log looks like.
struct Printable<'a>(&'a [u8]);

impl core::fmt::Display for Printable<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("\"")?;
        let mut utf8 = [0u8; 4];
        for &b in self.0 {
            let c = if (0x20..0x7F).contains(&b) && b != b'"' { b as char } else { '.' };
            f.write_str(c.encode_utf8(&mut utf8))?;
        }
        f.write_str("\"")
    }
}
/// Read `count` 4 KiB blocks at `lba`. On `Err` the transfer did not happen
/// and `buf` holds nothing the caller may believe.
/// The caller must be inside a block-device operation
/// ([`crate::block::begin_operation`]); a call with no budget established
/// above it is refused by name. [`BlockError::BudgetExpired`] is that
/// refusal; [`BlockError::Device`] is everything else.
pub fn storage_read(index: usize, lba: u64, count: u32, buf: &mut [u8]) -> BlockResult {
    with_disk(index, |ctrl, local| ctrl.msc_read(local, lba, count, buf))
        .unwrap_or(Err(BlockError::Device))
}

pub fn storage_write(index: usize, lba: u64, count: u32, buf: &[u8]) -> BlockResult {
    with_disk(index, |ctrl, local| ctrl.msc_write(local, lba, count, buf))
        .unwrap_or(Err(BlockError::Device))
}

pub fn storage_flush(index: usize) -> BlockResult {
    with_disk(index, |ctrl, local| ctrl.msc_flush(local)).unwrap_or(Err(BlockError::Device))
}

