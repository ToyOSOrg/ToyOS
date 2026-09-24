//! What a mass-storage device's transport is owed after a break: a ladder of
//! three rungs, each ended by the device's own answer and by nothing else.
//!
//! 1. [`Rung::ClassReset`] — Bulk-Only Transport 1.0 §5.3.4's Reset Recovery
//!    ([`crate::reset_recovery`]), which the class defines and a device may
//!    answer without honouring: §3.1 has it "ready … for the next CBW" once it
//!    answers the reset, and the requests give a host no way to tell a device
//!    that is from one that is still inside the command it was given.
//! 2. [`Rung::PortReset`] — the reset no USB device may ignore (USB 2.0 §9.1.1,
//!    Figure 9-1: every state returns to Default on it), and the enumeration
//!    that follows a reset ([`PORT_RESET`]), on the slot and under the disk
//!    number the device already has, so the volume on it carries on.
//! 3. [`Rung::Offline`] — the device did not answer after a port reset. Its
//!    endpoints are stopped and **the last thing it is sent is a reset**, so
//!    whatever command it was inside is over and the next host finds a device
//!    in its Default state.
//!
//! **Verified means a TEST UNIT READY answered under its own tag.** It is the
//! one command with no data phase, so it asks the device to move nothing.
//!
//! **A device owed Data-Out bytes is not given the class reset** ([`enters_at`]).
//! The rung's verification is itself 31 bytes on the Bulk-Out, and a device
//! that answered the class reset without leaving its data phase takes them as
//! the data it is owed: the question cannot be asked there without being part
//! of the answer. The port reset's can — after a reset and a
//! SET_CONFIGURATION no data phase is open (USB 2.0 §9.1.1, §9.4.7) — so that
//! is where such a break enters, and nothing reaches the Bulk-Out before it.
//!
//! **A rung is climbed once per run of breaks.** A rung that verified and whose
//! command then broke again has not brought the transport back, and asking it
//! again asks the same question of the same device: the next break climbs on.
//! Only a completed round trip ends the run.

/// One rung of the ladder.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Rung {
    ClassReset,
    PortReset,
    Offline,
}

impl Rung {
    /// What a line about this rung calls it.
    pub fn named(self) -> &'static str {
        match self {
            Self::ClassReset => "class reset",
            Self::PortReset => "port reset",
            Self::Offline => "last reset",
        }
    }
}

/// The most breaks a run can hold before its device is offline: one per rung.
pub const MOST_BREAKS: u8 = 3;

/// Where in its round trip a break left the device.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Left {
    /// Inside a Data-Out phase: it holds a command block and is owed bytes the
    /// host has stopped sending.
    OwedDataOut,
    /// Anywhere else: no command block taken, a Data-In it is sending, or a
    /// status it is holding.
    Elsewhere,
}

/// Where a break in `phase` of a command left the device; `data_out` is whether
/// the command has a Data-Out phase.
///
/// **A command-phase break of such a command is a device owed data too**: a CBW
/// whose handshake was lost after the device took it is a command the device
/// holds, and the bytes it then waits for are the Data-Out phase's. Only a break
/// in the status phase is past the data.
pub fn left(phase: crate::bot::Phase, data_out: bool) -> Left {
    use crate::bot::Phase;
    match phase {
        Phase::Command | Phase::DataOwed | Phase::Data if data_out => Left::OwedDataOut,
        _ => Left::Elsewhere,
    }
}

/// The lowest rung a break that left the device there may be answered with.
pub fn enters_at(left: Left) -> Rung {
    match left {
        Left::OwedDataOut => Rung::PortReset,
        Left::Elsewhere => Rung::ClassReset,
    }
}

/// The rung a break climbs to: above every rung this run of breaks has climbed
/// (`climbed` is the highest), and never below where the break itself enters.
/// A rung that did not verify is the next break, [`Left::Elsewhere`].
pub fn next(climbed: Option<Rung>, left: Left) -> Rung {
    let above = match climbed {
        None => Rung::ClassReset,
        Some(Rung::ClassReset) => Rung::PortReset,
        Some(Rung::PortReset | Rung::Offline) => Rung::Offline,
    };
    above.max(enters_at(left))
}

/// One step of [`Rung::PortReset`], in the order taken.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PortStep {
    /// Both bulk endpoints off their transfers (xHCI 1.2 §4.6.11: "software
    /// should stop all endpoint activity before issuing a Reset Device
    /// Command"). A pair that cannot be stopped does not end the rung: the
    /// Reset Device below terminates its activity and disables it.
    Quiesce,
    /// The most reset the port has ([`crate::port::offline_reset`]), waited
    /// for, its change flags consumed.
    Reset,
    /// The reset recovery time ([`RESET_RECOVERY_NS`]): the device owes no
    /// answer yet.
    Settle,
    /// Reset Device (§4.6.11): the slot to Default and address 0, every
    /// endpoint but the control endpoint Disabled — what the device now is.
    ResetDevice,
    /// Address Device with BSR clear, which §4.6.5 defines from the Default
    /// state: SET_ADDRESS, the first request the reset device answers.
    Address,
    /// SET_CONFIGURATION with the value the bind chose.
    Configure,
    /// Configure Endpoint adding the bulk pair on fresh rings, described as the
    /// bind described it.
    AddEndpoints,
}

/// [`Rung::PortReset`], up to the TEST UNIT READY it ends on. A step that fails
/// ends the rung unverified, [`PortStep::Quiesce`] alone excepted.
pub const PORT_RESET: [PortStep; 7] = [
    PortStep::Quiesce,
    PortStep::Reset,
    PortStep::Settle,
    PortStep::ResetDevice,
    PortStep::Address,
    PortStep::Configure,
    PortStep::AddEndpoints,
];

/// USB 2.0 §7.1.7.5 has a device answer 10 ms after a reset (TRSTRCY); this is
/// the 50 ms Linux's hub driver waits, for the firmware that needs it.
pub const RESET_RECOVERY_NS: u64 = 50_000_000;

/// Whether a failed `step` ends [`Rung::PortReset`].
pub fn ends_the_rung(step: PortStep) -> bool {
    step != PortStep::Quiesce
}

/// What a port reads after the rung's reset, against what the device was
/// enumerated as.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AfterReset {
    /// The same device at the same speed, its port enabled: it is addressed.
    Enumerate,
    /// The reset was written and its completion was not seen.
    NeverFinished,
    /// Nothing is connected: the device left, and its port's teardown owns
    /// what it held.
    Left,
    /// The reset completed and the port is not enabled (§4.19.5's failure).
    NotEnabled,
    /// It trained at another speed, so the endpoint descriptors the bind read
    /// no longer describe it.
    SpeedChanged { was: u8, now: u8 },
}

impl AfterReset {
    /// Whether the device was reset: the last thing it was sent is one.
    pub fn finished(self) -> bool {
        !matches!(self, Self::NeverFinished | Self::Left)
    }
}

impl core::fmt::Display for AfterReset {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Enumerate => f.write_str("reset, and the port is enabled"),
            Self::NeverFinished => f.write_str("the reset's completion was never seen"),
            Self::Left => f.write_str("nothing is connected, so its port's teardown takes it from here"),
            Self::NotEnabled => f.write_str("reset, and the port is not enabled"),
            Self::SpeedChanged { was, now } => {
                write!(f, "reset, and it trained at speed {now}, not the {was} it was enumerated at")
            }
        }
    }
}

/// `finished` is whether the reset's completion was seen; `connected`,
/// `enabled` and `speed` are the port's after it; `was` is the speed the device
/// was enumerated at.
pub fn after_reset(finished: bool, connected: bool, enabled: bool, speed: u8, was: u8) -> AfterReset {
    if !connected {
        AfterReset::Left
    } else if !finished {
        AfterReset::NeverFinished
    } else if !enabled {
        AfterReset::NotEnabled
    } else if speed != was {
        AfterReset::SpeedChanged { was, now: speed }
    } else {
        AfterReset::Enumerate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEFT: [Left; 2] = [Left::OwedDataOut, Left::Elsewhere];

    /// Every run, however its breaks left the device: no rung is climbed
    /// twice, none below where a break enters, the run is offline within
    /// [`MOST_BREAKS`], and offline is never reached before a port reset.
    #[test]
    fn every_run_of_breaks_climbs_strictly_and_ends_offline_after_a_port_reset() {
        for first in LEFT {
            for second in LEFT {
                for third in LEFT {
                    let mut climbed = None;
                    let mut rungs = [None; MOST_BREAKS as usize];
                    for (at, left) in [first, second, third].into_iter().enumerate() {
                        let rung = next(climbed, left);
                        assert!(climbed.is_none_or(|last| rung > last), "{climbed:?} then {rung:?}");
                        assert!(rung >= enters_at(left));
                        rungs[at] = Some(rung);
                        climbed = Some(rung);
                        if rung == Rung::Offline {
                            break;
                        }
                    }
                    assert_eq!(climbed, Some(Rung::Offline), "{first:?} {second:?} {third:?}");
                    let offline = rungs.iter().position(|r| *r == Some(Rung::Offline)).expect("reached");
                    assert_eq!(rungs[offline - 1], Some(Rung::PortReset), "{rungs:?}");
                }
            }
        }
    }

    #[test]
    fn a_break_elsewhere_climbs_every_rung() {
        assert_eq!(next(None, Left::Elsewhere), Rung::ClassReset);
        assert_eq!(next(Some(Rung::ClassReset), Left::Elsewhere), Rung::PortReset);
        assert_eq!(next(Some(Rung::PortReset), Left::Elsewhere), Rung::Offline);
    }

    #[test]
    fn a_device_owed_data_out_is_never_given_the_class_reset() {
        for climbed in [None, Some(Rung::ClassReset), Some(Rung::PortReset)] {
            assert_ne!(next(climbed, Left::OwedDataOut), Rung::ClassReset, "{climbed:?}");
        }
        assert_eq!(next(None, Left::OwedDataOut), Rung::PortReset);
    }

    /// A command that sends data leaves its device owed it from the moment its
    /// CBW may have been taken until its data phase is done; nothing else does.
    #[test]
    fn a_break_before_a_data_out_phase_is_done_leaves_the_device_owed_it() {
        use crate::bot::Phase;
        for phase in [Phase::Command, Phase::DataOwed, Phase::Data] {
            assert_eq!(left(phase, true), Left::OwedDataOut, "{phase:?}");
            assert_eq!(left(phase, false), Left::Elsewhere, "{phase:?} of a command sending nothing");
        }
        for phase in [Phase::Closed, Phase::StatusOwed, Phase::Status] {
            for data_out in [true, false] {
                assert_eq!(left(phase, data_out), Left::Elsewhere, "{phase:?}");
            }
        }
    }

    /// The order is the specifications': nothing reaches the device between
    /// the reset and its address, and nothing is configured before it is
    /// addressed.
    #[test]
    fn the_port_rung_resets_then_addresses_then_configures_then_asks() {
        let at = |step| PORT_RESET.iter().position(|s| *s == step).expect("a step of the rung");
        assert_eq!(at(PortStep::Quiesce), 0);
        assert!(at(PortStep::Reset) < at(PortStep::Settle));
        assert!(at(PortStep::Settle) < at(PortStep::ResetDevice));
        assert!(at(PortStep::ResetDevice) < at(PortStep::Address));
        assert!(at(PortStep::Address) < at(PortStep::Configure));
        assert!(at(PortStep::Configure) < at(PortStep::AddEndpoints));
        assert_eq!(at(PortStep::AddEndpoints), PORT_RESET.len() - 1);
    }

    #[test]
    fn only_a_quiesce_that_failed_lets_the_rung_go_on() {
        for step in PORT_RESET {
            assert_eq!(ends_the_rung(step), step != PortStep::Quiesce, "{step:?}");
        }
    }

    #[test]
    fn a_device_is_addressed_again_only_as_what_it_was_enumerated_as() {
        assert_eq!(after_reset(true, true, true, 3, 3), AfterReset::Enumerate);
        assert_eq!(after_reset(true, false, false, 0, 3), AfterReset::Left);
        assert_eq!(after_reset(true, false, true, 3, 3), AfterReset::Left, "connect outranks enable");
        assert_eq!(after_reset(false, true, true, 3, 3), AfterReset::NeverFinished);
        assert_eq!(after_reset(true, true, false, 3, 3), AfterReset::NotEnabled);
        assert_eq!(after_reset(true, true, true, 4, 3), AfterReset::SpeedChanged { was: 3, now: 4 });
        for left in [AfterReset::NeverFinished, AfterReset::Left] {
            assert!(!left.finished(), "{left:?}");
        }
        for left in [AfterReset::Enumerate, AfterReset::NotEnabled, AfterReset::SpeedChanged { was: 3, now: 4 }] {
            assert!(left.finished(), "{left:?}");
        }
    }
}
