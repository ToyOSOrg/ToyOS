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
/// **The one place that question is answered**, because the boot scan and the
/// hot-plug machine must answer it the same way: the laptop's stick is in the
/// port when the machine boots, so a fix that only reached the hot-plug path
/// would not reach the machine it is for.
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

/// The reset a device gets from a class driver that has given up on it: the
/// most the port has. A warm reset does everything a hot one does and also
/// takes the link through Rx.Detect (§4.19.5.1), and only a USB3 port has one.
pub fn offline_reset(protocol: Option<Protocol>) -> Reset {
    match protocol {
        Some(Protocol::Usb3) => Reset::Warm,
        Some(Protocol::Usb2) | None => Reset::Hot,
    }
}

/// What a port does with a device that was reset because its class driver gave
/// up on it, once the device's slot is back with the controller.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AfterOffline {
    /// Enumerate it as a fresh connect. §4.19.5 leaves a reset device
    /// answering address 0 and has software address it or disable the port
    /// "immediately"; the enumeration is also the only thing that says whether
    /// the device the reset left is one a host can use.
    Enumerate,
    /// This connection was enumerated again once already: the port stays
    /// attached, so nothing enumerates the device a third time.
    StayAttached,
}

/// The PORTSC write that performs `reset`.
pub fn reset_write(reset: Reset, portsc: Portsc) -> portsc::Write {
    match reset {
        Reset::Hot => portsc.neutral().resetting(),
        Reset::Warm => portsc.neutral().warm_resetting(),
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
    /// This connection's device was taken offline and enumerated again once;
    /// cleared by the teardown of a device that really left.
    revived: bool,
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
        revived: false,
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
        self.revived = false;
    }

    /// What this port owes a device its class driver took offline and reset:
    /// one more enumeration per connection, and no second.
    pub fn taken_offline(&mut self) -> AfterOffline {
        if core::mem::replace(&mut self.revived, true) {
            AfterOffline::StayAttached
        } else {
            AfterOffline::Enumerate
        }
    }

    /// The offline device's slot is back with the controller and
    /// [`Self::taken_offline`] said [`AfterOffline::Enumerate`]: the port is
    /// empty as far as the driver is concerned, so the next look runs the
    /// fresh-connect path over the device that is still in it.
    pub fn revive(&mut self) {
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
            if portsc.reset_changed() {
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
                    // An empty port that reads empty carries no connection for
                    // [`Self::taken_offline`] to have spent: a device whose reset
                    // moved it to another port leaves this one without a teardown.
                    if !connected {
                        self.revived = false;
                    }
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

    const CCS: u32 = 1 << 0;
    const PED: u32 = 1 << 1;
    const PP: u32 = 1 << 9;

    #[test]
    fn a_device_given_up_on_gets_the_most_reset_its_port_has() {
        assert_eq!(offline_reset(Some(Protocol::Usb3)), Reset::Warm);
        // WPR is RsvdZ on a USB2 protocol port (§4.19.5.1's note), and a port
        // the controller did not describe is driven the USB2 way everywhere.
        assert_eq!(offline_reset(Some(Protocol::Usb2)), Reset::Hot);
        assert_eq!(offline_reset(None), Reset::Hot);
    }

    fn bound(protocol: Protocol) -> PortState {
        let mut port = PortState::EMPTY;
        port.speaks(Some(protocol));
        port.adopt(NonZeroU8::new(5));
        port
    }

    #[test]
    fn an_offline_device_is_enumerated_again_once_per_connection() {
        let mut port = bound(Protocol::Usb3);
        assert_eq!(port.taken_offline(), AfterOffline::Enumerate);
        port.revive();
        assert!(!port.attached() && port.slot().is_none());

        // The link the warm reset left is up, so the fresh-connect path
        // debounces and enumerates with no reset of its own.
        let up = Portsc::from_raw(CCS | PED | PP | (4 << 10));
        assert!(matches!(port.step(up, 0), Step::Wait(at) if at == DEBOUNCE_NS));
        match port.step(up, DEBOUNCE_NS) {
            Step::Enumerate { after: None, pending } => drop(pending.running()),
            other => panic!("{other:?}"),
        }
        port.enumerated(NonZeroU8::new(6));

        // The same connection, given up on again: nothing enumerates it a
        // third time.
        assert_eq!(port.taken_offline(), AfterOffline::StayAttached);
        assert!(port.attached());
        assert!(matches!(port.step(up, 2 * DEBOUNCE_NS), Step::Idle));
    }

    #[test]
    fn a_usb2_device_is_reset_by_the_fresh_connect_it_is_enumerated_as() {
        let mut port = bound(Protocol::Usb2);
        assert_eq!(port.taken_offline(), AfterOffline::Enumerate);
        port.revive();
        let enabled = Portsc::from_raw(CCS | PED | PP | (3 << 10));
        assert!(matches!(port.step(enabled, 0), Step::Wait(_)));
        assert!(matches!(port.step(enabled, DEBOUNCE_NS), Step::Reset(Reset::Hot, _)));
    }

    #[test]
    fn a_real_unplug_gives_the_next_connection_its_own_enumeration() {
        let mut port = bound(Protocol::Usb3);
        assert_eq!(port.taken_offline(), AfterOffline::Enumerate);
        port.revive();
        port.enumerated(NonZeroU8::new(6));
        port.torn_down();
        port.adopt(NonZeroU8::new(7));
        assert_eq!(port.taken_offline(), AfterOffline::Enumerate);
    }

    #[test]
    fn a_device_whose_reset_moved_it_to_another_port_leaves_this_one_fresh() {
        // T14 run 74: the stick was on the USB2 half of its receptacle, the
        // offline reset sent it to the USB3 half, and this port read empty
        // with no device of its own to tear down.
        let mut port = bound(Protocol::Usb2);
        assert_eq!(port.taken_offline(), AfterOffline::Enumerate);
        port.revive();
        assert!(matches!(port.step(Portsc::from_raw(PP), 0), Step::Idle));
        port.adopt(NonZeroU8::new(8));
        assert_eq!(port.taken_offline(), AfterOffline::Enumerate);
    }
}
