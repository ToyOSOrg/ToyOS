//! What the driver does about one root-hub port, as a decision and nothing
//! else.
//!
//! Contract: the caller reads PORTSC, hands it here with the clock, and does
//! exactly what comes back — then reads PORTSC again before the next step,
//! because an effect changes the register and a decision taken from a stale
//! word is a decision about a port that no longer exists.

use core::num::NonZeroU8;

use crate::portsc::{self, LinkState, Portsc};
use crate::protocol::Protocol;

/// Nanoseconds since boot, as the caller counts them.
pub type Nanos = u64;

/// How long a connect state must hold still before the driver acts on it.
///
/// USB 2.0 §7.1.7.3 requires 100 ms between an attach being detected and the
/// port reset that follows it — TATTDB, which Linux's `hub_port_debounce`
/// calls `HUB_DEBOUNCE_STABLE`.
pub const DEBOUNCE_NS: Nanos = 100_000_000;

/// How long a reset may take before the port is given up on.
///
/// Policy, and the caller's transfer budget: a register bit the controller sets
/// in microseconds and has not set in two seconds belongs to a port this driver
/// cannot drive.
pub const RESET_DEADLINE_NS: Nanos = 2_000_000_000;

/// Which reset a connected port needs before anything can be enumerated on it,
/// or `None` when its link is already up and there is nothing to do.
///
/// **The one place that question is answered**: the hot-plug machine asks it,
/// and the boot scan asks [`inherited_reset`], which answers the same for
/// every port but a trained link.
pub fn reset_needed(protocol: Option<Protocol>, portsc: Portsc) -> Option<Reset> {
    if protocol != Some(Protocol::Usb3) {
        // USB2, or a port the controller did not describe. A reset is how a
        // device gets enabled there at all.
        return Some(Reset::Hot);
    }
    // A USB3 link trains itself: §4.19.1.2 has the port reach Enabled on its
    // own, so a port that reads Enabled has a working link and nothing to
    // reset. Resetting it anyway is a hot reset of a trained link, and a link
    // that cannot take one lands Inactive.
    if portsc.enabled() {
        return None;
    }
    // Inactive is the state only a warm reset leaves (§4.19.1.2.4), so go
    // straight to it rather than spending a deadline proving a hot one fails.
    Some(if portsc.link_state() == LinkState::Inactive { Reset::Warm } else { Reset::Hot })
}

/// Which reset a device already connected when this kernel brings its controller
/// up needs: **always one**, and on a link that reads trained the warm one.
///
/// Such a device is whatever the firmware's driver left it: addressed,
/// configured, possibly inside a Bulk-Only command. HCRST drives no reset on the
/// bus (xHCI 1.2 §5.4.1), so the controller's own reset tells the device
/// nothing; SET_ADDRESS to a device in the Configured state is not specified
/// (USB 2.0 §9.4.6), and every enumeration the USB specification describes
/// begins with a port reset (USB 2.0 §9.1.2). So nothing is asked of it until
/// its port has been reset and this kernel has enumerated it from its Default
/// state. A trained link gets [`Reset::Warm`] because its link state is as
/// unknown as its protocol state, and a warm reset does everything a hot one
/// does and also retrains the link (§4.19.5.1). [`reset_needed`] still answers
/// for a link this kernel watched train: its device came up from power-on.
pub fn inherited_reset(protocol: Option<Protocol>, portsc: Portsc) -> Reset {
    reset_needed(protocol, portsc).unwrap_or(Reset::Warm)
}

/// The reset a device gets from a class driver its own recovery did not bring
/// back ([`crate::ladder`]): the most the port has. A warm reset does everything
/// a hot one does and also takes the link through Rx.Detect (§4.19.5.1), and
/// only a USB3 port has one.
pub fn offline_reset(protocol: Option<Protocol>) -> Reset {
    match protocol {
        Some(Protocol::Usb3) => Reset::Warm,
        Some(Protocol::Usb2) | None => Reset::Hot,
    }
}

/// The PORTSC write that performs `reset`, clearing any reset-finished flag an
/// earlier reset left, so the one that comes up next is this reset's.
pub fn reset_write(reset: Reset, portsc: Portsc) -> portsc::Write {
    let ack = portsc.neutral().acknowledging_reset(portsc);
    match reset {
        Reset::Hot => ack.resetting(),
        Reset::Warm => ack.warm_resetting(),
    }
}

/// The write an enumeration opens with: the reset flags it consumed, and for a
/// warm completion the retrain's own connect edge (§4.19.5.1), which left set
/// reads as a replug and cancels the enumeration it belongs to. Here rather
/// than at either caller, because a simulator whose acknowledge is a second
/// copy of the driver's certifies a driver nobody ships.
pub fn enumeration_ack(after: Option<Reset>, portsc: Portsc) -> portsc::Write {
    let ack = portsc.neutral().acknowledging_reset(portsc);
    match after {
        Some(Reset::Warm) => ack.acknowledging_connect(portsc),
        _ => ack,
    }
}

/// Why a port stopped being worked on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GaveUp {
    /// A reset was written and no completion came.
    ResetNeverFinished(Reset),
    /// A USB3 link was warm-reset as well and still did not come up.
    /// §4.19.1.2 has nothing beyond a warm reset, so this is the end of the
    /// road for the port rather than one step short of it.
    LinkNeverTrained,
    /// The reset *completed* with the port still disabled, where §4.19.5
    /// offers no escalation ("USB2 protocol ports never fail"): a controller
    /// misbehaving, not a link a warm reset could retrain.
    ResetFailed(Reset),
}

/// What a completed reset means for the port — **the one place that question
/// is answered**, for [`reset_needed`]'s reason: §4.19.5's failure signature
/// (PRC set, the port still disabled) must route the boot scan and the
/// hot-plug machine to the same §4.19.5.1 escalation.
pub fn reset_outcome(kind: Reset, protocol: Option<Protocol>, portsc: Portsc) -> ResetOutcome {
    if portsc.enabled() {
        return ResetOutcome::Enumerate;
    }
    if kind == Reset::Hot && protocol == Some(Protocol::Usb3) {
        // The failure word's change flags are the failed reset's own artifacts,
        // and the reset-and-enumerate that follows already handles any real
        // replug they could witness — so the escalation consumes them.
        return ResetOutcome::Escalate(portsc.neutral().warm_resetting().acknowledging(portsc));
    }
    ResetOutcome::GaveUp(match kind {
        Reset::Warm => GaveUp::LinkNeverTrained,
        Reset::Hot => GaveUp::ResetFailed(Reset::Hot),
    })
}

/// [`reset_outcome`]'s answer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResetOutcome {
    /// The reset enabled the port: enumerate what is on it.
    Enumerate,
    /// It failed: write this — §4.19.5.1's warm reset — and wait again.
    Escalate(portsc::Write),
    /// It failed with no escalation left.
    GaveUp(GaveUp),
}

/// Which reset a port is being given.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reset {
    /// PR. What enables a USB2 port at all, and on a USB3 port a *hot* reset of
    /// a link that has already trained.
    Hot,
    /// WPR. A full re-training of the link, USB3 only, and the only way out of
    /// [`crate::LinkState::Inactive`] — the state a USB3 port lands in when it
    /// cannot take the hot reset a USB2-shaped driver gives it.
    Warm,
}

/// Why a port's device is being taken down.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Gone {
    /// The port has read disconnected for a whole debounce.
    Disconnected,
    /// It reads connected, but CSC says it stopped being connected in between:
    /// whatever is in the port now, the device that was here has gone.
    Replugged,
}

/// What the driver must do for this port before it looks at it again.
#[derive(Debug)]
pub enum Step<'a> {
    /// Nothing to do. The port reads the way the driver left it.
    Idle,
    /// Come back at this instant and no sooner.
    Wait(Nanos),
    /// Write this to PORTSC.
    Write(portsc::Write),
    /// Reset the port by writing this. Separate from an ordinary write because
    /// **which reset a port needs is the decision this machine exists to
    /// make** — a function of what the port speaks and what its link is doing,
    /// not three fixes bolted onto one another.
    Reset(Reset, portsc::Write),
    /// Take down whatever this port had.
    Teardown(Gone, Pending<'a>),
    /// Bring up whatever is in this port. `after` names the reset this
    /// enumeration follows, `None` for a link that arrived trained; the kind
    /// matters to the caller's acknowledge, since a warm completion carries
    /// the retrain's own connect edge (§4.19.5.1).
    Enumerate { after: Option<Reset>, pending: Pending<'a> },
    /// Say this and leave the port alone until its device is pulled.
    GaveUp(GaveUp),
}

/// An effect the caller was told to perform and has not yet begun.
///
/// It borrows the machine, so nothing can be decided about this port until the
/// caller has said the effect is under way. **The effect itself needs the
/// controller, which is what the machine's own owner is inside** — so the
/// borrow ends at [`Pending::running`] and the outstanding effect becomes
/// [`Work::Working`], which is a state a re-entrant step can be caught by.
/// Re-entering a port's decision from inside its own enumeration is reachable,
/// because enumeration drains the event ring, and it used to be prevented only
/// by an invariant nothing stated.
#[derive(Debug)]
pub struct Pending<'a>(&'a mut PortState);

impl Pending<'_> {
    /// The effect has begun. Nothing is decided about this port again until it
    /// is reported through [`PortState::enumerated`] or
    /// [`PortState::torn_down`].
    pub fn running(self) -> Effect {
        let effect = match self.0.work {
            Work::Resetting { .. } => Effect::Enumerating,
            _ => Effect::TearingDown,
        };
        self.0.work = Work::Working(effect);
        effect
    }
}

/// An effect that has begun and has not been reported.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Effect {
    Enumerating,
    TearingDown,
}

/// What the driver is part-way through doing about this port.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Work {
    /// The port reads the way the driver last acted on it.
    Settled,
    /// Its connect state differs from the one the driver acted on, and has
    /// since `at`. A port that changes its mind again gets a fresh `at`.
    Debouncing { at: Nanos },
    /// A reset is written and no completion has arrived. `kind` is which one,
    /// because it decides what happens when the deadline passes: a hot reset a
    /// USB3 port did not take is what a warm reset is *for*, and a warm one
    /// that failed is the end.
    Resetting { until: Nanos, kind: Reset },
    /// The caller is inside an effect and has not reported it.
    Working(Effect),
}

/// A deliberate defect, compiled only for the negative gates.
///
/// Each names one decision below that every positive scenario in the simulator
/// would still pass without. A gate that cannot fail proves nothing, so the
/// simulator turns each of these on in turn and requires the run to go red.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Flaw {
    #[default]
    None,
    /// Compare CCS against what the driver believes and ignore CSC.
    IgnoreConnectChange,
    /// Clear the change flags before deciding what they meant.
    AcknowledgeBeforeDeciding,
    /// Write back the whole word that was read.
    WriteBackWhatWasRead,
    /// Restart the debounce clock on every observation.
    RestartDebounce,
    /// Wait for a reset for as long as it takes.
    NoResetDeadline,
}

/// One root-hub port, as the driver believes it.
#[derive(Clone, Copy, Debug)]
pub struct PortState {
    /// The connect state the driver last acted on, and deliberately not the
    /// register's: a change is measured against this.
    attached: bool,
    /// The slot the controller enabled for this port, which the driver owns
    /// until it issues Disable Slot. `Option<NonZeroU8>` because slot ids are
    /// 1-based, so a zero would be a sentinel where the niche makes the option
    /// the same byte.
    slot: Option<NonZeroU8>,
    work: Work,
    /// What this port speaks, or `None` where the controller did not say.
    /// Unknown is driven the USB2 way, which is what every port got before the
    /// Supported Protocol capability was read at all.
    protocol: Option<Protocol>,
    #[cfg(feature = "flaws")]
    flaw: Flaw,
}

impl Default for PortState {
    fn default() -> Self {
        Self::EMPTY
    }
}

impl PortState {
    pub const EMPTY: Self = Self {
        attached: false,
        slot: None,
        work: Work::Settled,
        protocol: None,
        #[cfg(feature = "flaws")]
        flaw: Flaw::None,
    };

    /// The slot this port's device holds, which is what a teardown gives back.
    pub fn slot(&self) -> Option<NonZeroU8> {
        self.slot
    }

    /// Take the slot, for a caller that is about to disable it.
    ///
    /// The port stays attached, which is deliberate and not an oversight: a
    /// port whose belief goes empty with the device still physically in it
    /// reads as a fresh connect on the next pass, and the driver would
    /// enumerate the same endpoint again every debounce for as long as it
    /// stayed plugged in.
    pub fn take_slot(&mut self) -> Option<NonZeroU8> {
        self.slot.take()
    }

    /// The connect state the driver has acted on.
    pub fn attached(&self) -> bool {
        self.attached
    }

    /// Whether this port is waiting on something rather than at rest.
    pub fn outstanding(&self) -> bool {
        self.work != Work::Settled
    }

    /// The effect the caller began and has not reported, if any. A step taken
    /// while this is `Some` is a re-entrant one.
    pub fn working(&self) -> Option<Effect> {
        match self.work {
            Work::Working(effect) => Some(effect),
            _ => None,
        }
    }

    /// The enumeration finished and the controller enabled `slot`, or it did
    /// not. Recorded either way: an Enable Slot that succeeded is the
    /// controller's resource whatever happened after it.
    pub fn enumerated(&mut self, slot: Option<NonZeroU8>) {
        self.adopt(slot);
    }

    /// The teardown finished. The port is empty as far as the driver is
    /// concerned, whatever the register says, so the next look runs the
    /// ordinary fresh-connect path.
    pub fn torn_down(&mut self) {
        self.attached = false;
        self.slot = None;
        self.work = Work::Settled;
    }

    /// Adopt a port the boot scan enumerated, so the hot-plug machine starts
    /// from what the boot path already did rather than re-deciding it.
    /// What the controller's Supported Protocol capability said this port
    /// speaks. Set once, at bring-up, from firmware's own description of the
    /// machine.
    pub fn speaks(&mut self, protocol: Option<Protocol>) {
        self.protocol = protocol;
    }

    pub fn adopt(&mut self, slot: Option<NonZeroU8>) {
        self.attached = true;
        self.slot = slot;
        self.work = Work::Settled;
    }

    #[cfg(feature = "flaws")]
    pub fn with_flaw(flaw: Flaw) -> Self {
        Self { flaw, ..Self::EMPTY }
    }

    /// Whether this driver was staged with `flaw`: a flawed acknowledge is
    /// flawed at the simulator's effect sites too, not only in the machine.
    #[cfg(feature = "flaws")]
    pub fn has_flaw(&self, flaw: Flaw) -> bool {
        self.flawed(flaw)
    }

    #[cfg(feature = "flaws")]
    fn flawed(&self, flaw: Flaw) -> bool {
        self.flaw == flaw
    }

    /// Always false in a kernel build: the field a flaw would be read from does
    /// not exist there, so no production path can reach one.
    #[cfg(not(feature = "flaws"))]
    fn flawed(&self, _flaw: Flaw) -> bool {
        false
    }

    /// What to do about this port, given what it reads now.
    pub fn step(&mut self, portsc: Portsc, now: Nanos) -> Step<'_> {
        let connected = portsc.connected();
        let replugged = portsc.connect_changed() && !self.flawed(Flaw::IgnoreConnectChange);

        // A step taken from inside an effect this port is already having done
        // to it. Nothing to decide — the caller has not finished the last
        // decision — and `invariants::check` is what calls it what it is.
        if matches!(self.work, Work::Working(_)) {
            return Step::Idle;
        }

        // Before any change flag is cleared, because PRC is the one this state
        // is waiting for.
        if let Work::Resetting { until, kind } = self.work {
            if portsc.reset_finished() {
                match reset_outcome(kind, self.protocol, portsc) {
                    ResetOutcome::Enumerate => {
                        return Step::Enumerate { after: Some(kind), pending: Pending(self) };
                    }
                    ResetOutcome::Escalate(write) => {
                        self.work =
                            Work::Resetting { until: now + RESET_DEADLINE_NS, kind: Reset::Warm };
                        return Step::Reset(Reset::Warm, write);
                    }
                    ResetOutcome::GaveUp(why) => {
                        self.attached = true;
                        self.work = Work::Settled;
                        return Step::GaveUp(why);
                    }
                }
            }
            if now < until || self.flawed(Flaw::NoResetDeadline) {
                return Step::Wait(until);
            }
            // **A hot reset a USB3 port would not take is what a warm reset
            // exists for.** The link is Inactive or never left Polling, and
            // §4.19.1.2.4 has exactly one way out of that. A driver without it
            // stops here, which is a USB-A port that never mounts anything.
            if kind == Reset::Hot && self.protocol == Some(Protocol::Usb3) && connected {
                self.work = Work::Resetting { until: now + RESET_DEADLINE_NS, kind: Reset::Warm };
                return Step::Reset(Reset::Warm, reset_write(Reset::Warm, portsc));
            }
            // Attached, so the port is not tried again until its device is
            // pulled — which is what stops a port the controller will not reset
            // from being reset forever.
            self.attached = true;
            self.work = Work::Settled;
            return Step::GaveUp(match kind {
                Reset::Warm => GaveUp::LinkNeverTrained,
                Reset::Hot => GaveUp::ResetNeverFinished(Reset::Hot),
            });
        }

        if self.flawed(Flaw::AcknowledgeBeforeDeciding) && portsc.any_change() {
            return Step::Write(portsc.neutral().acknowledging(portsc));
        }

        // **Ahead of the acknowledge, because the acknowledge is what destroys
        // the evidence.** The port reads connected and the driver already
        // believed it was, so nothing below would act — but CSC says the
        // connection was broken in between, and whatever is in the port now,
        // the device that was here is gone. The ordinary teardown then sets
        // `attached` false, which turns the rest of this into the fresh connect
        // it already knows how to run.
        if connected && replugged && self.attached {
            return Step::Teardown(Gone::Replugged, Pending(self));
        }

        if portsc.any_change() {
            #[cfg(feature = "flaws")]
            let write = if self.flawed(Flaw::WriteBackWhatWasRead) {
                portsc::Write::whole_word(portsc)
            } else {
                portsc.neutral().acknowledging(portsc)
            };
            #[cfg(not(feature = "flaws"))]
            let write = portsc.neutral().acknowledging(portsc);
            return Step::Write(write);
        }

        let held = match self.work {
            Work::Settled => {
                if connected == self.attached {
                    return Step::Idle;
                }
                self.work = Work::Debouncing { at: now };
                return Step::Wait(now + DEBOUNCE_NS);
            }
            // It changed its mind back: nothing to act on, and the next change
            // starts a fresh debounce.
            Work::Debouncing { .. } if connected == self.attached => {
                self.work = Work::Settled;
                return Step::Idle;
            }
            Work::Debouncing { .. } if self.flawed(Flaw::RestartDebounce) => {
                self.work = Work::Debouncing { at: now };
                0
            }
            Work::Debouncing { at } => now.saturating_sub(at),
            Work::Resetting { .. } | Work::Working(_) => unreachable!("returned above"),
        };
        if held < DEBOUNCE_NS {
            return Step::Wait(now + DEBOUNCE_NS - held);
        }
        if !connected {
            return Step::Teardown(Gone::Disconnected, Pending(self));
        }

        let Some(kind) = reset_needed(self.protocol, portsc) else {
            return Step::Enumerate { after: None, pending: Pending(self) };
        };
        self.work = Work::Resetting { until: now + RESET_DEADLINE_NS, kind };
        Step::Reset(kind, reset_write(kind, portsc))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PORTSC with CCS and PP set, PED as given, and `pls` as the link state.
    fn connected(enabled: bool, pls: u32) -> Portsc {
        Portsc::from_raw(1 | (u32::from(enabled) << 1) | (pls << 5) | (1 << 9) | (4 << 10))
    }

    /// T14 run 86: port 13's warm reset was judged in the same millisecond it
    /// was asked for, on a word with PR still set and a PRC nobody had cleared,
    /// and the port given up as a link that would not train. The write clears
    /// such a flag, the word is not a finished reset, and run 84's completion
    /// word on the same port is.
    #[test]
    fn a_warm_reset_is_finished_only_once_the_controller_has_cleared_pr() {
        const PR: u32 = 1 << 4;
        const PRC: u32 = 1 << 21;
        const WRC: u32 = 1 << 19;
        const WPR: u32 = 1 << 31;
        // Trained, with a reset-finished flag left by whatever ran before.
        let before = Portsc::from_raw(0x0020_1203);
        assert_eq!(inherited_reset(Some(Protocol::Usb3), before), Reset::Warm);
        let write = reset_write(Reset::Warm, before).raw();
        assert_eq!(write & (WPR | PRC), WPR | PRC, "{write:#010x}: the stale flag is cleared with it");
        assert_eq!(write & (1 << 1), 0, "{write:#010x} would disable the port");

        // Run 86's word, in the millisecond the reset was asked for.
        let during = Portsc::from_raw(0x0022_12b1);
        assert_eq!(during.raw() & (PR | PRC), PR | PRC);
        assert!(!during.reset_finished(), "PR is still set: the reset is on the wire");

        // Run 84's word after port 13's warm reset: PR clear, PRC and WRC set, enabled.
        let after = Portsc::from_raw(0x0028_1203);
        assert_eq!(after.raw() & (PR | PRC | WRC), PRC | WRC);
        assert!(after.reset_finished());
        assert_eq!(reset_outcome(Reset::Warm, Some(Protocol::Usb3), after), ResetOutcome::Enumerate);
    }

    #[test]
    fn a_device_found_at_bring_up_is_never_enumerated_without_a_reset() {
        let trained = connected(true, 0);
        assert_eq!(reset_needed(Some(Protocol::Usb3), trained), None, "a link watched training");
        assert_eq!(inherited_reset(Some(Protocol::Usb3), trained), Reset::Warm);
        // Everything else is answered as it always was: the question is only
        // whether a reset is skipped.
        assert_eq!(inherited_reset(Some(Protocol::Usb3), connected(false, 7)), Reset::Hot);
        assert_eq!(inherited_reset(Some(Protocol::Usb3), connected(false, 6)), Reset::Warm);
        assert_eq!(inherited_reset(Some(Protocol::Usb2), connected(true, 0)), Reset::Hot);
        assert_eq!(inherited_reset(None, trained), Reset::Hot);
    }

    #[test]
    fn a_device_its_class_reset_did_not_bring_back_gets_the_most_reset_its_port_has() {
        assert_eq!(offline_reset(Some(Protocol::Usb3)), Reset::Warm);
        // WPR is RsvdZ on a USB2 protocol port (§4.19.5.1's note), and a port
        // the controller did not describe is driven the USB2 way everywhere.
        assert_eq!(offline_reset(Some(Protocol::Usb2)), Reset::Hot);
        assert_eq!(offline_reset(None), Reset::Hot);
    }
}
