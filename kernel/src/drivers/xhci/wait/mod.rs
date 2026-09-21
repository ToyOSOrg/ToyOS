//! Everything in this driver that waits, in the three contexts where it is
//! correct: [`boot`]'s scan (no scheduler pass yet exists), [`msc`]'s disk
//! reads/writes (the faulting thread spends its own time), and
//! [`msc::bind`]'s bring-up (the one blocking call a scheduler pass may still
//! reach). `wait_command`, `wait_transfer`, `settles`, `run_command`,
//! `control_transfer`, `restart_endpoint` and `settle_outstanding` are private
//! to this module, so `xhci`, `xhci::device` and `xhci::hid` cannot reach them
//! from a scheduler pass.

pub mod boot;
pub mod msc;

/// Deepest preempt depth seen while waiting for a disk transfer; a
/// measurement only.
#[cfg(feature = "boot-actuators")]
mod depth_probe {
    use core::sync::atomic::{AtomicU32, Ordering};

    use crate::log;

    static DEEPEST: AtomicU32 = AtomicU32::new(0);

    pub fn report() {
        let depth = crate::preempt::count();
        // Logs only a new deepest depth: logging every wait would write to
        // the same device the wait is on, a self-sustaining loop.
        if depth <= DEEPEST.fetch_max(depth, Ordering::Relaxed) {
            return;
        }
        log!(
            "io-depth: a disk transfer is being waited for at preempt depth {depth}, task {:?}",
            crate::arch::percpu::current_tid().map(|t| t.raw())
        );
        let rbp: u64;
        // SAFETY: reads the frame pointer; `kernel_backtrace` stops at the first unreadable frame.
        unsafe { core::arch::asm!("mov {}, rbp", out(reg) rbp, options(nomem, nostack)) };
        crate::arch::idt::exceptions::kernel_backtrace(rbp, 20);
    }
}

use crate::log;
use super::{deadline, enqueue_control, log_unrecoverable, Completion, Trb, TrbRing};
use super::{XhciController, EVENT_TRANSFER, EVENT_CMD_COMPLETE, USB_TIMEOUT_NS};
use super::{CC_SUCCESS, CC_SHORT_PACKET};
use toyos_xhci::call::NotTaken;
use toyos_xhci::job::Await;
use toyos_xhci::recovery::{Act, NeedsConfigure, Recovery};

/// How one control transfer ended; `Done` carries the bytes actually moved,
/// since the completion code alone cannot say.
#[derive(Clone, Copy)]
enum Control {
    /// Both stages completed; `delivered` is zero for a transfer with no data stage.
    Done { delivered: u16 },
    /// The controller reported `code` for the named stage.
    Failed { stage: &'static str, code: u32 },
    /// Nothing came back for the named stage, for [`Quiet`]'s reason.
    Silent { stage: &'static str, why: Quiet },
    /// Not put on the ring: the call it belongs to may spend nothing more.
    NotSent(NotTaken),
}

/// Why a wait ended with no event; only [`Quiet::Elapsed`] spent the timeout budget.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Quiet {
    /// [`USB_TIMEOUT_NS`] passed with the port still connected.
    Elapsed,
    /// The port reads disconnected.
    Gone,
    /// The call's budget after a break ran out before this wait's own timeout did.
    Spent,
    /// A staged break skipped the wait by design.
    #[cfg(feature = "boot-actuators")]
    Staged,
}

impl Quiet {
    /// What to say about a step that ended this way; `step` is the caller's word for it.
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
            Self::Spent => write!(
                f,
                "this call's budget after a break ran out during the {step} {kind}"
            ),
            #[cfg(feature = "boot-actuators")]
            Self::Staged => write!(f, "a staged break skipped the {step} {kind} wait"),
        }
    }
}

impl Control {
    /// Whether the transfer completed.
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
            Self::NotSent(why) => write!(f, "not sent: {why}"),
        }
    }
}

/// One endpoint, as [`XhciController::restart_endpoint`] needs to see it.
struct Restart<'a> {
    slot_id: u8,
    /// The device block whose output context carries this endpoint's state.
    ctx_block: usize,
    dci: u8,
    /// The address the *device* knows this endpoint by, for CLEAR_FEATURE.
    ep_addr: u8,
    /// Where in the pool the transfer ring lives; recovery rebuilds it rather
    /// than resuming a ring the controller has a stale dequeue pointer into.
    ring_at: usize,
    ring: &'a mut TrbRing,
    ep0_ring: &'a mut TrbRing,
}

/// Spin until `ready`, and say whether that happened inside [`USB_TIMEOUT_NS`].
fn settles(ready: impl Fn() -> bool) -> bool {
    crate::clock::settles(USB_TIMEOUT_NS, ready)
}

impl XhciController {
    /// Now, and when a wait starting now gives up: on its own timeout, or where the call's budget after a break ends.
    fn wait_ends(&self) -> (u64, u64) {
        let now = crate::clock::nanos_since_boot();
        (now, self.after_break.wait_ends(now, USB_TIMEOUT_NS))
    }

    /// [`settles`], inside what the call may still spend.
    fn settles_within_call(&self, ready: impl Fn() -> bool) -> bool {
        let now = crate::clock::nanos_since_boot();
        crate::clock::settles(self.after_break.wait_left(now, USB_TIMEOUT_NS), ready)
    }

    /// Take one endpoint back to a state that runs TRBs, waiting for each step.
    fn restart_endpoint(&mut self, ep: Restart<'_>) -> bool {
        let halt = Some((ep.ep_addr, ep.ep0_ring));
        self.run_recovery(ep.slot_id, ep.dci, ep.ctx_block, ep.ring, ep.ring_at, halt)
    }

    /// [`Recovery`] run to its end, one blocking command at a time. `halt` is
    /// the address the device knows the endpoint by and the EP0 ring a
    /// CLEAR_FEATURE for it goes out on, or `None` for an endpoint with no Halt
    /// feature to clear.
    #[allow(clippy::too_many_arguments)]
    fn run_recovery(
        &mut self,
        slot_id: u8,
        dci: u8,
        ctx_block: usize,
        ring: &mut TrbRing,
        ring_at: usize,
        halt: Option<(u8, &mut TrbRing)>,
    ) -> bool {
        let slot = self.slot(slot_id);
        let state = self.endpoint_state(ctx_block, dci);
        log!("xHCI: {slot} endpoint {dci} is {state}, recovering");
        let (mut seq, mut act) = match Recovery::begin(state) {
            Ok(begun) => begun,
            Err(NeedsConfigure(state)) => {
                log_unrecoverable(slot, dci, state);
                return false;
            }
        };
        loop {
            let cmd = match act {
                Act::Running => return true,
                // The last act of every route that has one.
                Act::ClearHalt => {
                    return match halt {
                        Some((ep_addr, ep0_ring)) => {
                            self.clear_endpoint_halt(slot_id, ctx_block, ep0_ring, ep_addr)
                        }
                        None => true,
                    }
                }
                Act::Command(cmd) => cmd,
            };
            let trb = self.recovery_trb(cmd, slot_id, dci, ring, ring_at);
            if !self.run_command(trb, cmd.name()) {
                return false;
            }
            act = seq.completed();
        }
    }

    /// The default control pipe, back to a state that runs TRBs. Its own
    /// entry point: USB 2.0 §9.4.5 defines no Halt feature for the default
    /// pipe, so only the controller's half (xHCI 1.2 §4.6.8) applies.
    fn restart_control_endpoint(
        &mut self,
        slot_id: u8,
        ctx_block: usize,
        ring: &mut TrbRing,
    ) -> bool {
        self.run_recovery(
            slot_id,
            super::EP0_DCI,
            ctx_block,
            ring,
            ctx_block + super::DEV_EP0_RING,
            None,
        )
    }

    /// CLEAR_FEATURE(ENDPOINT_HALT) for the endpoint the device knows as
    /// `ep_addr`: the half of a recovery the **device** answers.
    fn clear_endpoint_halt(
        &mut self,
        slot_id: u8,
        ctx_block: usize,
        ep0_ring: &mut TrbRing,
        ep_addr: u8,
    ) -> bool {
        let cleared = self
            .control_transfer(slot_id, ctx_block, ep0_ring, 0x02, 0x01, 0, ep_addr as u16, None, 0);
        if !cleared.done() {
            log!("xHCI: {} would not clear the halt on endpoint {ep_addr:#04x}: {cleared}",
                self.slot(slot_id));
            return false;
        }
        true
    }

    /// Run whatever is outstanding to its end, waiting for each answer; the
    /// boot scan only, before `init` publishes the controller for anything else to poll.
    fn settle_outstanding(&mut self) {
        // **Bounded on silence and not on work.** Neither loop's exit is this
        // driver's — `busy` clears when the controller answers and `broke_with`
        // when a recovery takes — so a controller that answers neither is a boot
        // scan that never returns, before there is a scheduler to preempt it. A
        // scan that is still being answered is not stuck, though, and a bound on
        // its *total* time is one a crowded bus reaches honestly: this one is
        // re-armed by every event the controller produces.
        let mut quiet_until = deadline();
        // Also loops on `broke_with`: a halted endpoint raises no further
        // interrupt, so a first-transfer failure would otherwise go
        // unrecovered for the rest of boot.
        while self.outstanding.busy() || self.devices.iter().any(|d| d.broke_with.is_some()) {
            if crate::clock::nanos_since_boot() >= quiet_until {
                log!("xHCI: the boot scan heard nothing for {} ms with work still outstanding; \
                     the rest of this boot goes on without it", USB_TIMEOUT_NS / 1_000_000);
                return;
            }
            self.recover_endpoints();
            while self.outstanding.busy() {
                if self.drain_events() {
                    quiet_until = deadline();
                } else if crate::clock::nanos_since_boot() >= quiet_until {
                    break;
                }
                self.advance_outstanding();
                core::hint::spin_loop();
            }
        }
    }

    /// The completion code and slot id of the command that was enqueued at
    /// `trb`, or `None` if the controller never answered. Matched by the TRB
    /// address, not the next completion event, per Command Completion Event's
    /// own addressing (xHCI 1.2 §6.4.2.2).
    fn wait_command(&mut self, trb: u64) -> Option<(u32, u32)> {
        let (_, deadline) = self.wait_ends();
        loop {
            // **Every iteration, not only an empty ring.** A ring that keeps
            // producing events this wait is not waiting for made the bound
            // unreachable, and the caller holds `XHCI` and a block operation
            // with preemption off for the whole of it.
            if crate::clock::nanos_since_boot() >= deadline {
                return None;
            }
            let Some(event) = self.next_event() else {
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
    /// anything it did not under `what`'s name.
    fn run_command(&mut self, trb: Trb, what: &str) -> bool {
        match self.command_code(trb, what) {
            Some(CC_SUCCESS) => true,
            Some(code) => {
                log!("xHCI: {what} failed: {}", Completion(code));
                false
            }
            None => false,
        }
    }

    /// Submit `trb` and hand back the completion code, for a caller with its
    /// own answer to one; `None` is a controller that never answered, which is
    /// logged here.
    fn command_code(&mut self, trb: Trb, what: &str) -> Option<u32> {
        let began = crate::clock::nanos_since_boot();
        if let Err(why) = self.after_break.command(began) {
            log!("xHCI: {what} not issued: {why}");
            return None;
        }
        let at = self.submit_command(trb);
        match self.wait_command(at) {
            Some((code, _)) => Some(code),
            None => {
                self.after_break.unanswered();
                // The controller's own word beside the silence: a command ring
                // that stopped (CRCR.CRR clear) or a Host Controller Error
                // (USBSTS.HCE) is a controller no further command reaches.
                log!(
                    "xHCI: {what} timed out after {} ms with USBSTS={:#010x} and CRCR.CRR={}",
                    (crate::clock::nanos_since_boot() - began) / 1_000_000,
                    self.op_base.read_u32(super::OP_USBSTS),
                    (self.op_base.read_u64(super::OP_CRCR) >> 3) & 1
                );
                None
            }
        }
    }

    /// The completion of the transfer queued at `trb` on (`slot`, `dci`), as a
    /// completion code and the number of bytes the controller did *not* move.
    /// Matched by (slot, dci, trb) rather than the endpoint, since a stalled
    /// endpoint still completes late transfers this driver stopped waiting for.
    fn wait_transfer(&mut self, slot: u8, dci: u8, trb: u64) -> Result<(u32, u32), Quiet> {
        #[cfg(feature = "boot-actuators")]
        if crate::actuator::io_depth_probe() {
            depth_probe::report();
        }
        // `usb-reset-break` stages a Reset Recovery control transfer to answer
        // nothing; see `msc::reset_break`.
        #[cfg(feature = "boot-actuators")]
        if msc::reset_break::active() {
            return Err(Quiet::Staged);
        }
        let on = Await::Transfer { slot, dci, trb };
        let (began, deadline) = self.wait_ends();
        let port = self.port_of_slot(slot);
        loop {
            // **Every iteration, not only an empty ring**; see `wait_command`.
            let now = crate::clock::nanos_since_boot();
            if now >= deadline {
                let cut = self.after_break.cut(began, now, USB_TIMEOUT_NS);
                return Err(if cut { Quiet::Spent } else { Quiet::Elapsed });
            }
            let Some(event) = self.next_event() else {
                // An unplugged device is not a slow one; the timeout budget is
                // for a port that might still answer.
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
            // Not this wait's event: forwarded so a bound device's own
            // interrupt ring stays fed rather than dropped here.
            self.dispatch_event(event);
        }
    }

    /// One control transfer on `ring`, which must be the EP0 ring named by
    /// `slot`'s device context.
    // Nine arguments: a USB setup packet has five fields, which is what a
    // caller varies (xHCI 1.2 §6.4.1.2.1).
    #[allow(clippy::too_many_arguments)]
    fn control_transfer(
        &mut self,
        slot: u8,
        ctx_block: usize,
        ring: &mut TrbRing,
        bm_request_type: u8,
        b_request: u8,
        w_value: u16,
        w_index: u16,
        data_buf: Option<u64>,
        data_len: u16,
    ) -> Control {
        if let Err(why) = self.after_break.request(crate::clock::nanos_since_boot()) {
            return Control::NotSent(why);
        }
        let trbs = enqueue_control(
            ring, bm_request_type, b_request, w_value, w_index, data_buf, data_len,
        );
        self.ring_doorbell(slot, 1);

        let mut delivered = 0u16;
        if let Some(data) = trbs.data {
            match self.wait_transfer(slot, 1, data) {
                Ok((CC_SUCCESS | CC_SHORT_PACKET, residue)) => {
                    // Residue is clamped: past the requested length it would
                    // report more bytes delivered than the buffer holds.
                    delivered = data_len.saturating_sub(residue.min(u16::MAX as u32) as u16);
                }
                // An errored data stage halts EP0, so the status stage's TRB
                // never runs; it is not waited for.
                Ok((code, _)) => {
                    self.recover_after(slot, ctx_block, ring, code);
                    return Control::Failed { stage: "data", code };
                }
                Err(why) => return Control::Silent { stage: "data", why },
            }
        }
        match self.wait_transfer(slot, 1, trbs.status) {
            Ok((CC_SUCCESS, _)) => Control::Done { delivered },
            Ok((code, _)) => {
                self.recover_after(slot, ctx_block, ring, code);
                Control::Failed { stage: "status", code }
            }
            Err(why) => Control::Silent { stage: "status", why },
        }
    }

    /// Take EP0 back out of Halted where `code` says the device stalled the
    /// transfer, before the failure reaches a caller likely to send another one.
    fn recover_after(&mut self, slot: u8, ctx_block: usize, ring: &mut TrbRing, code: u32) {
        if code != super::CC_STALL {
            return;
        }
        if !self.restart_control_endpoint(slot, ctx_block, ring) {
            log!("xHCI: {} EP0 stayed halted after the stall", self.slot(slot));
        }
    }

}
