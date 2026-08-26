//! Everything in this driver that waits, and the three contexts where waiting
//! is correct.
//!
//! **The split is a module and not a type.**
//! `poll_if_pending` runs at the top of every scheduler pass on every
//! CPU, so nothing it reaches may spin on a device: a `USB_TIMEOUT_NS` spent
//! there is spent by every CPU that enters a pass, and pulling the boot stick
//! out of a laptop aims a filesystem sync, a page-cache fill and the scheduler at
//! the same dead device.
//!
//! A view handed to the poll would not have enforced it. Rust makes a module's
//! private items visible to its *descendants*, so `xhci::device` and
//! `xhci::hid` can name anything `xhci` keeps private — including a view's own
//! field. The primitives therefore live **below** the poll rather than beside
//! it: `wait_command`, `wait_transfer`, `settles`, `run_command`,
//! `control_transfer`, `restart_endpoint` and `settle_outstanding` are private
//! to this module, and `xhci`, `xhci::device` and `xhci::hid` are not inside
//! it. A pass that tried to wait does not compile.
//!
//! The three descendants are the three contexts:
//!
//! - [`boot`] — the scan. There is no scheduler yet, so the pass this would
//!   give itself back to does not exist.
//! - [`msc`] — a disk. `storage_read` and `storage_write` run on the thread
//!   that faulted, which is spending its own time.
//! - [`msc::bind`] — **the one door**, and the only blocking thing a scheduler
//!   pass can still reach. A disk plugged in after boot has to be brought up by
//!   somebody, its bring-up is Bulk-Only Transport, and that is a machine of
//!   its own that has not been written yet
//!   (`issues/hardware/the-bot-scsi-machine-is-still-hand-written-in-the-kernel.md`).
//!   Until then the claim above holds of everything except a disk arriving
//!   after boot.
//!
//! # Two bounds, and only one of them is this driver's
//!
//! [`USB_TIMEOUT_NS`] bounds *one* command or transfer, and it is reached only
//! by a device that has stopped answering. What a caller actually spends is the
//! composition above it — `ceil(count / MSC_MAX_BLOCKS)` commands, each of them
//! issued up to [`msc`]'s `MAX_TRANSPORT_ATTEMPTS` times with a Reset Recovery
//! between the attempts — and nothing in this driver has an opinion about how
//! long that may be. [`crate::block::OPERATION`] is that opinion, and it
//! belongs to the layer that knows one call is one operation.
//!
//! **It arrives ambiently and is threaded from there.** The
//! deadline is established on the running context above `BlockDevice` and
//! recovered by [`msc`]'s three operation entry points — `msc_read`,
//! `msc_write`, `msc_flush` — because the two frames in between cannot carry
//! it. From those three down it is an ordinary argument, ending at
//! `XhciController::scsi`, which is the one site that reads it; that is what
//! leaves `scsi` usable by `msc::bind`'s bring-up, which is not a block-device
//! operation, has no establishment above it, and passes [`Deadline::never`] by
//! name. `block::OPERATION`'s doc carries why the refusal is taken between
//! commands and never inside one.
//!
//! [`Deadline::never`]: crate::time::Deadline::never

pub mod boot;
pub mod msc;

/// How much of the kernel is holding a spinlock at the moment a device is
/// waited for.
///
/// A measurement and not an actuator: nothing here changes what the driver
/// does. It exists because the depth cannot be read off the call graph — the
/// backtrace it prints beside it is what says which locks those are, and one of
/// them is named nowhere in the chain of function names. The work in
/// `issues/kernel/every-wait-in-this-kernel-is-a-spin.md` is judged on this
/// number falling.
///
/// Deepest-so-far rather than every wait, because a line per transfer on a
/// machine whose log lives on the transfer's own device is the self-sustaining
/// write loop [`msc::MscDevice`]'s `no_write_cache` already records.
#[cfg(feature = "boot-actuators")]
mod depth_probe {
    use core::sync::atomic::{AtomicU32, Ordering};

    use crate::log;

    static DEEPEST: AtomicU32 = AtomicU32::new(0);

    pub fn report() {
        let depth = crate::preempt::count();
        if depth <= DEEPEST.fetch_max(depth, Ordering::Relaxed) {
            return;
        }
        log!(
            "io-depth: a disk transfer is being waited for at preempt depth {depth}, task {:?}",
            crate::arch::percpu::current_tid().map(|t| t.raw())
        );
        let rbp: u64;
        // SAFETY: reading the frame pointer. `kernel_backtrace` walks the chain
        // defensively and stops at the first frame it cannot read.
        unsafe { core::arch::asm!("mov {}, rbp", out(reg) rbp, options(nomem, nostack)) };
        crate::arch::idt::exceptions::kernel_backtrace(rbp, 20);
    }
}

use crate::log;
use super::{deadline, enqueue_control, log_unrecoverable, Completion, Trb, TrbRing};
use super::{XhciController, EVENT_TRANSFER, EVENT_CMD_COMPLETE, USB_TIMEOUT_NS};
use super::{CC_SUCCESS, CC_SHORT_PACKET};
use toyos_xhci::job::Await;
use toyos_xhci::recovery::{Act, NeedsConfigure, Recovery};

/// How one control transfer ended.
///
/// `Done` carries the bytes the device actually moved, because the completion
/// code cannot say: the Status Stage reports Success whether the Data Stage
/// filled the buffer or left it untouched, so a `GET_DESCRIPTOR` that returned
/// nothing and one that returned all 18 bytes are otherwise the same answer.
///
/// Three variants and no `Option`: a device that never answered carries no
/// completion code at all, and a type that conflates it with one makes every
/// failure line ambiguous.
#[derive(Clone, Copy)]
enum Control {
    /// Both stages completed. `delivered` is what the device moved in the data
    /// stage, and zero for a transfer that has none.
    Done { delivered: u16 },
    /// The controller reported `code` for the named stage.
    Failed { stage: &'static str, code: u32 },
    /// Nothing came back for the named stage, for [`Quiet`]'s reason.
    Silent { stage: &'static str, why: Quiet },
}

/// Why a wait ended with no event.
///
/// **A wait that gave up is not necessarily a wait that elapsed**, and the
/// difference is where a triage goes: a pulled stick reported as a slow one
/// sends its reader after the controller. Only [`Quiet::Elapsed`] spent the
/// budget, and only it may say so.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Quiet {
    /// [`USB_TIMEOUT_NS`] passed with the port still connected.
    Elapsed,
    /// The port reads disconnected. Nothing is behind it, and no budget makes
    /// that answer arrive.
    Gone,
    /// A staged break skipped the wait by design.
    #[cfg(feature = "boot-actuators")]
    Staged,
}

impl Quiet {
    /// What to say about a step that ended this way. `step` is the caller's
    /// word for it — `"data"`, `"status"` — and `kind` is `"phase"` or
    /// `"stage"`, so no formatter here allocates on a driver's error path.
    pub(super) fn about(
        self,
        step: &str,
        kind: &str,
        f: &mut core::fmt::Formatter<'_>,
    ) -> core::fmt::Result {
        match self {
            Self::Elapsed => write!(
                f,
                "no answer in the {step} {kind} in {} ms",
                USB_TIMEOUT_NS / 1_000_000
            ),
            Self::Gone => write!(f, "the port disconnected during the {step} {kind}"),
            #[cfg(feature = "boot-actuators")]
            Self::Staged => write!(f, "a staged break skipped the {step} {kind} wait"),
        }
    }
}

impl Control {
    /// Whether the transfer completed, for the requests that carry no data
    /// stage and so have no byte count to check.
    fn done(self) -> bool {
        matches!(self, Self::Done { .. })
    }
}

impl core::fmt::Display for Control {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Done { delivered } => write!(f, "{delivered} B delivered"),
            Self::Failed { stage, code } => write!(f, "{stage} stage completion {}", Completion(*code)),
            Self::Silent { stage, why } => why.about(stage, "stage", f),
        }
    }
}

/// One endpoint, as [`XhciController::restart_endpoint`] needs to see it.
///
/// A struct because the two callers hold their endpoints in different shapes —
/// a disk's bulk pair live in a mass-storage pool block and a HID's interrupt
/// endpoint in a device block — and the recovery needs both the block the
/// controller writes the *output context* into and the place the ring's memory
/// is. Passing them positionally is six numbers whose order is the whole
/// contract.
struct Restart<'a> {
    slot_id: u8,
    /// The device block whose output context carries this endpoint's state.
    ctx_block: usize,
    dci: u8,
    /// The address the *device* knows this endpoint by, which is what a
    /// CLEAR_FEATURE names.
    ep_addr: u8,
    /// Where in the pool the transfer ring lives, because recovery rebuilds it
    /// rather than resuming a ring the controller has a stale dequeue pointer
    /// into.
    ring_at: usize,
    ring: &'a mut TrbRing,
    ep0_ring: &'a mut TrbRing,
}

/// Spin until `ready`, and say whether that happened inside [`USB_TIMEOUT_NS`].
///
/// The register bits this covers are ones the controller sets in microseconds;
/// one that never sets belongs to a controller or a port this driver cannot
/// drive, and every caller turns `false` into a refusal that names it. An
/// unbounded `spin_loop` in its place, on a machine with no serial port, is the
/// same picture as every other way a boot can stop: `Boot: peripherals ready`
/// painted on the panel, forever.
///
/// The wait itself is [`crate::clock::settles`]; what this adds is the bound,
/// which is the only part of it that is the driver's own.
fn settles(ready: impl Fn() -> bool) -> bool {
    crate::clock::settles(USB_TIMEOUT_NS, ready)
}


/// What one endpoint's recovery still owes the **device** once the controller
/// has taken it off the transfer it was running.
///
/// A value of this is what [`XhciController::quiesce_endpoint`] produces and
/// the only thing [`XhciController::clear_endpoint_halt`] accepts, so the two
/// halves of a recovery cannot be run in the other order. That matters to
/// exactly one caller: Bulk-Only Transport's Reset Recovery has a device reset
/// of its own to put between them, and it may not issue that reset while either
/// endpoint still holds a transfer the driver stopped waiting for — see
/// [`Act::ClearHalt`].
#[derive(Clone, Copy)]
pub(in crate::drivers::xhci) enum Owed {
    /// The endpoint runs. Nothing further is owed.
    Nothing,
    /// The device is still holding a halt on the endpoint at this address.
    ClearHalt { ep_addr: u8 },
    /// The endpoint could not be taken off its transfer. Carried rather than
    /// reported by returning early, because the *other* endpoint and the
    /// device's own reset still have to happen — leaving one endpoint stopped
    /// because a step on the other failed is what turns a recoverable device
    /// into a permanently offline one.
    Failed,
}

impl XhciController {
    /// Take one endpoint back to a state that runs TRBs, waiting for each step.
    ///
    /// The route is [`Recovery`]'s and the effects are here. **This driver of
    /// it blocks, and that is correct for its one caller**: a disk's bulk pair
    /// is recovered from `storage_read`/`storage_write`, on the thread that
    /// faulted, which is spending its own time. A HID endpoint is recovered at
    /// the top of a scheduler pass, where it would be spending everybody's, and
    /// [`Self::step_recovery`] is the same route stepped across passes.
    fn restart_endpoint(&mut self, mut ep: Restart<'_>) -> bool {
        let owed = self.quiesce_endpoint(&mut ep);
        self.clear_endpoint_halt(ep.slot_id, ep.ep0_ring, owed)
    }

    /// The half of one endpoint's recovery the **controller** answers: every
    /// command [`Recovery`] owes, up to the point where the sequence would
    /// speak to the device.
    fn quiesce_endpoint(&mut self, ep: &mut Restart<'_>) -> Owed {
        let slot = self.slot(ep.slot_id);
        let state = self.endpoint_state(ep.ctx_block, ep.dci);
        log!("xHCI: {slot} endpoint {} is {state}, recovering", ep.dci);
        let (mut seq, mut act) = match Recovery::begin(state) {
            Ok(begun) => begun,
            Err(NeedsConfigure(state)) => {
                log_unrecoverable(slot, ep.dci, state);
                return Owed::Failed;
            }
        };
        loop {
            let cmd = match act {
                Act::Running => return Owed::Nothing,
                Act::ClearHalt => return Owed::ClearHalt { ep_addr: ep.ep_addr },
                Act::Command(cmd) => cmd,
            };
            let trb = self.recovery_trb(cmd, ep.slot_id, ep.dci, ep.ring, ep.ring_at);
            if !self.run_command(trb, cmd.name()) {
                return Owed::Failed;
            }
            act = seq.completed();
        }
    }

    /// The half the **device** answers, which is the only packet a recovery
    /// puts on the bus.
    fn clear_endpoint_halt(&mut self, slot_id: u8, ep0_ring: &mut TrbRing, owed: Owed) -> bool {
        let ep_addr = match owed {
            Owed::Nothing => return true,
            Owed::Failed => return false,
            Owed::ClearHalt { ep_addr } => ep_addr,
        };
        let cleared =
            self.control_transfer(slot_id, ep0_ring, 0x02, 0x01, 0, ep_addr as u16, None, 0);
        if !cleared.done() {
            log!("xHCI: {} would not clear the halt on endpoint {ep_addr:#04x}: {cleared}",
                self.slot(slot_id));
            return false;
        }
        true
    }

    /// Run whatever is outstanding to its end, waiting for each answer.
    ///
    /// **Blocking is correct here and only here**: this is the boot scan, so
    /// there is no scheduler yet and the pass this would otherwise give itself
    /// back to does not exist, and `init` has not published the controller for
    /// anything else to poll it. An endpoint holding no TRB raises no further
    /// interrupt, so a device whose *first* transfer failed during the scan
    /// would otherwise stay recorded and silent for the whole boot.
    ///
    /// It is also the boot scan's driver of the enumeration sequence, and the
    /// only difference between it and the hot-plug one: the same acts, run one
    /// after another here and one per scheduler pass there.
    fn settle_outstanding(&mut self) {
        while self.outstanding.busy() || self.devices.iter().any(|d| d.broke_with.is_some()) {
            self.recover_endpoints();
            while self.outstanding.busy() {
                while let Some(event) = self.next_event() {
                    self.dispatch_event(event);
                }
                self.advance_outstanding();
                core::hint::spin_loop();
            }
        }
    }

    /// The completion code and slot id of the command that was enqueued at
    /// `trb`, or `None` if the controller never answered.
    ///
    /// **The address and not the next completion of any command.** A Command
    /// Completion Event names its Command TRB (§6.4.2.2), and a driver that
    /// took the first one it saw would hand a command that had run out its
    /// deadline and answered afterwards to whoever asked next — which a
    /// scheduler pass, leaving a command behind, makes reachable.
    fn wait_command(&mut self, trb: u64) -> Option<(u32, u32)> {
        let deadline = deadline();
        loop {
            let Some(event) = self.next_event() else {
                if crate::clock::nanos_since_boot() >= deadline {
                    return None;
                }
                core::hint::spin_loop();
                continue;
            };
            if (event.control >> 10) & 0x3F == EVENT_CMD_COMPLETE && event.param & !0xF == trb {
                return Some(((event.status >> 24) & 0xFF, (event.control >> 24) & 0xFF));
            }
            self.dispatch_event(event);
        }
    }

    /// Submit `trb` and say whether the controller accepted it, logging
    /// anything it did not. `what` names the command in that line, because a
    /// bare code is unreadable at 3am.
    ///
    /// A `bool` and not an `Option<u32>`: the only code a caller acts on is
    /// `CC_SUCCESS`, so a richer return would pretend a question was open.
    fn run_command(&mut self, trb: Trb, what: &str) -> bool {
        let at = self.submit_command(trb);
        match self.wait_command(at) {
            Some((CC_SUCCESS, _)) => true,
            Some((code, _)) => {
                log!("xHCI: {what} failed: {}", Completion(code));
                false
            }
            None => {
                log!("xHCI: {what} timed out");
                false
            }
        }
    }

    /// The completion of the transfer queued at `trb` on (`slot`, `dci`), as a
    /// completion code and the number of bytes the controller did *not* move.
    ///
    /// The event ring is one queue for the whole controller, so anything that
    /// arrives here and is not ours belongs to a bound device delivering a
    /// report — handing it to `dispatch_event` rather than dropping it is what
    /// keeps that device's interrupt ring fed.
    ///
    /// **The TRB and not the endpoint**, which is [`Await::Transfer`]'s own
    /// argument arriving at the site that motivated it: one slot carries three
    /// endpoints, a stalled one still completes, and a transfer this driver
    /// stopped waiting for is still the device's to answer. Matching on
    /// (slot, dci) alone hands that late answer — and its residue, which is how
    /// many of the caller's bytes are real — to whatever asked next on the same
    /// endpoint.
    fn wait_transfer(&mut self, slot: u8, dci: u8, trb: u64) -> Result<(u32, u32), Quiet> {
        #[cfg(feature = "boot-actuators")]
        if crate::actuator::io_depth_probe() {
            depth_probe::report();
        }
        // The staged hung recovery: this wait's transfer is one of the Reset
        // Recovery control transfers `usb-reset-break` covers, and a device
        // that answers nothing is staged by not waiting for the answer. See
        // `msc::reset_break` — the window is open only inside the one staged
        // `reset_recovery` call, on this CPU, under the lock this wait holds.
        #[cfg(feature = "boot-actuators")]
        if msc::reset_break::active() {
            return Err(Quiet::Staged);
        }
        let on = Await::Transfer { slot, dci, trb };
        let deadline = deadline();
        let port = self.port_of_slot(slot);
        loop {
            let Some(event) = self.next_event() else {
                if crate::clock::nanos_since_boot() >= deadline {
                    return Err(Quiet::Elapsed);
                }
                // **A device that has been unplugged is not a device that is
                // slow.** The budget exists for one that might still answer; a
                // port that reads disconnected has nothing behind it, and every
                // nanosecond spent proving that is spent holding `XHCI` with
                // preemption disabled — on the path a filesystem sync, a
                // page-cache fill and every scheduler pass all take. Pulling
                // the stick a machine logs to aims all three at a dead device
                // on the same event.
                if port.is_some_and(|p| !self.read_portsc(p).connected()) {
                    return Err(Quiet::Gone);
                }
                core::hint::spin_loop();
                continue;
            };
            let trb_type = (event.control >> 10) & 0x3F;
            let answers = Await::Transfer {
                slot: ((event.control >> 24) & 0xFF) as u8,
                dci: ((event.control >> 16) & 0x1F) as u8,
                trb: event.param & !0xF,
            };
            if trb_type == EVENT_TRANSFER && answers == on {
                return Ok(((event.status >> 24) & 0xFF, event.status & 0x00FF_FFFF));
            }
            self.dispatch_event(event);
        }
    }

    /// One control transfer on `ring`, which must be the EP0 ring named by
    /// `slot`'s device context.
    // Nine arguments because a USB setup packet has five fields and those are
    // what a caller varies; a struct for them would name the wire format a
    // second time (xHCI 1.2 §6.4.1.2.1).
    #[allow(clippy::too_many_arguments)]
    fn control_transfer(
        &mut self,
        slot: u8,
        ring: &mut TrbRing,
        bm_request_type: u8,
        b_request: u8,
        w_value: u16,
        w_index: u16,
        data_buf: Option<u64>,
        data_len: u16,
    ) -> Control {
        let trbs = enqueue_control(
            ring, bm_request_type, b_request, w_value, w_index, data_buf, data_len,
        );
        self.ring_doorbell(slot, 1);

        let mut delivered = 0u16;
        if let Some(data) = trbs.data {
            match self.wait_transfer(slot, 1, data) {
                Ok((CC_SUCCESS | CC_SHORT_PACKET, residue)) => {
                    // A residue past the length asked for is a controller
                    // contradicting itself; believing it would report more bytes
                    // delivered than the buffer holds.
                    delivered = data_len.saturating_sub(residue.min(u16::MAX as u32) as u16);
                }
                // The status stage is deliberately not waited for. An errored
                // data stage halts EP0, so the TRB behind it never runs, and
                // waiting would spend the whole transfer budget learning that.
                Ok((code, _)) => return Control::Failed { stage: "data", code },
                Err(why) => return Control::Silent { stage: "data", why },
            }
        }
        match self.wait_transfer(slot, 1, trbs.status) {
            Ok((CC_SUCCESS, _)) => Control::Done { delivered },
            Ok((code, _)) => Control::Failed { stage: "status", code },
            Err(why) => Control::Silent { stage: "status", why },
        }
    }

}
