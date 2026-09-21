//! Where one Bulk-Only Transport command stands, and whose status a status
//! block is ([`whose`]).
//!
//! USB Mass Storage Class Bulk-Only Transport 1.0 §5.1 makes every command
//! three transfers — the 31-byte CBW out, an optional data phase, the 13-byte
//! CSW in — and §6.7.2/§6.7.3 leave a device that has taken a CBW waiting for
//! whichever of them has not arrived. A host that stops between two phases has
//! told the device nothing, so which phase it stopped in is the only thing that
//! says what the device is holding.
//!
//! The phases are here and their effects are the driver's, because the reader
//! is the reset path: it runs where no lock may be taken, so it reads the phase
//! out of an atomic rather than out of the driver.

/// Where one Bulk-Only round trip stands, published by the driver as it walks
/// the three phases.
///
/// **Five and not three**, though two pairs leave the device the same thing:
/// which of a pair a stopped machine is in is the difference between a device
/// the controller was still serving and one nothing had asked yet, and the
/// reset's account is the only place that is ever said.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Phase {
    /// No command is open. Between round trips, and the state a device is left
    /// in by one that completed.
    #[default]
    Closed,
    /// The CBW's transfer is queued and unanswered: the device may already hold
    /// the command and may not.
    Command,
    /// The CBW is answered and the data phase's transfer is not queued yet.
    DataOwed,
    /// The data phase's transfer is queued and unanswered.
    Data,
    /// The data phase is done, or there was none, and the CSW's transfer is not
    /// queued yet.
    StatusOwed,
    /// The CSW's transfer is queued; only the device's answer is outstanding.
    Status,
}

impl Phase {
    /// The byte the driver publishes and the reset path reads back.
    ///
    /// A number of this module's own and not a discriminant cast, so a phase
    /// added between two existing ones cannot renumber what a reader in another
    /// crate is decoding.
    pub fn code(self) -> u8 {
        match self {
            Self::Closed => 0,
            Self::Command => 1,
            Self::DataOwed => 2,
            Self::Data => 3,
            Self::StatusOwed => 4,
            Self::Status => 5,
        }
    }

    /// The phase `code` names, or [`Self::Closed`] for a byte no phase
    /// publishes — the reset path may not panic on what it reads.
    pub fn of(code: u8) -> Self {
        match code {
            1 => Self::Command,
            2 => Self::DataOwed,
            3 => Self::Data,
            4 => Self::StatusOwed,
            5 => Self::Status,
            _ => Self::Closed,
        }
    }

    /// Whether a device is inside a command here.
    pub fn open(self) -> bool {
        self != Self::Closed
    }

    /// What a line about this phase calls it — the driver's failure lines and
    /// the reset's account, so a reader never has to line two spellings up.
    pub fn named(self) -> &'static str {
        match self {
            Self::Closed => "no",
            Self::Command => "command",
            Self::DataOwed => "data (unqueued)",
            Self::Data => "data",
            Self::StatusOwed => "status (unqueued)",
            Self::Status => "status",
        }
    }
}

impl core::fmt::Display for Phase {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.named())
    }
}

/// Whose status a status block is, by its tag.
///
/// §5.1 has the device echo `dCBWTag` in `dCSWTag`, which "positively
/// associates a CSW with the corresponding CBW". A host that stopped waiting
/// for a command never took that command's status, and §3.1's reset is the only
/// thing the class offers to make the device forget it; a device that answers
/// the reset and keeps the status hands it to whoever reads the Bulk-In next.
/// The tag is what tells that reader so.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Whose {
    /// The command the status was read for.
    Ours,
    /// A command this host sent before that one and never took the status of:
    /// the device is a status behind its host.
    Abandoned,
    /// No command this host is still owed a status for.
    Nobodys,
}

/// `csw` is the tag the status carries, `ours` the tag of the command it was
/// read for, and `answered` the newest tag whose own status this host has
/// taken: every tag after `answered` and before `ours` went out and was given
/// up on. Tags count upward and wrap.
///
/// A caller that reads on past an abandoned status passes that status's tag as
/// the next `answered`, so the same status twice is nobody's and the reading
/// ends within the commands that were really abandoned.
pub fn whose(csw: u32, ours: u32, answered: u32) -> Whose {
    let back = ours.wrapping_sub(csw);
    if back == 0 {
        Whose::Ours
    } else if back < ours.wrapping_sub(answered) {
        Whose::Abandoned
    } else {
        Whose::Nobodys
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVERY: [Phase; 6] = [
        Phase::Closed,
        Phase::Command,
        Phase::DataOwed,
        Phase::Data,
        Phase::StatusOwed,
        Phase::Status,
    ];

    #[test]
    fn every_phase_survives_the_byte_it_is_published_as() {
        for phase in EVERY {
            assert_eq!(Phase::of(phase.code()), phase);
        }
        for (at, phase) in EVERY.iter().enumerate() {
            for other in &EVERY[at + 1..] {
                assert_ne!(
                    phase.code(),
                    other.code(),
                    "{phase:?} and {other:?} publish the same byte"
                );
            }
        }
    }

    #[test]
    fn a_byte_no_phase_publishes_reads_as_closed() {
        // The reader is the reset path, where a refusal that panicked would take
        // the machine down inside the act that is handing it back.
        for code in 6..=u8::MAX {
            assert_eq!(Phase::of(code), Phase::Closed, "code {code}");
        }
    }

    #[test]
    fn every_phase_but_the_closed_one_is_a_device_holding_a_command() {
        assert!(!Phase::Closed.open());
        for phase in EVERY.iter().copied().filter(|p| *p != Phase::Closed) {
            assert!(phase.open(), "{phase:?}");
        }
    }

    #[test]
    fn the_status_the_bench_read_is_the_abandoned_writes() {
        // T14 runs 74 and 75: WRITE(10) 0x58a abandoned, the next command
        // 0x58b, and the status that came back carried 0x58a.
        assert_eq!(whose(0x58a, 0x58b, 0x589), Whose::Abandoned);
        assert_eq!(whose(0x58b, 0x58b, 0x589), Whose::Ours);
    }

    #[test]
    fn a_status_is_abandoned_only_inside_the_commands_given_up_on() {
        let (answered, ours) = (10u32, 14u32);
        for csw in 0..32u32 {
            let want = match csw {
                14 => Whose::Ours,
                11..=13 => Whose::Abandoned,
                _ => Whose::Nobodys,
            };
            assert_eq!(whose(csw, ours, answered), want, "tag {csw}");
        }
        // Nothing abandoned: the last command's status, a second time, is
        // nobody's.
        assert_eq!(whose(10, 11, 10), Whose::Nobodys);
    }

    #[test]
    fn reading_on_ends_within_the_commands_given_up_on() {
        // A device that repeats the oldest status for ever: the caller moves
        // `answered` up to each abandoned status it takes, so the repeat is
        // nobody's.
        let ours = 14u32;
        let mut answered = 10u32;
        let mut taken = 0;
        loop {
            let csw = 11;
            match whose(csw, ours, answered) {
                Whose::Abandoned => {
                    answered = csw;
                    taken += 1;
                }
                other => {
                    assert_eq!(other, Whose::Nobodys);
                    break;
                }
            }
        }
        assert_eq!(taken, 1);
    }

    #[test]
    fn tags_wrap() {
        let answered = u32::MAX - 1;
        let ours = 1u32;
        assert_eq!(whose(u32::MAX, ours, answered), Whose::Abandoned);
        assert_eq!(whose(0, ours, answered), Whose::Abandoned);
        assert_eq!(whose(1, ours, answered), Whose::Ours);
        assert_eq!(whose(answered, ours, answered), Whose::Nobodys);
        assert_eq!(whose(2, ours, answered), Whose::Nobodys);
    }

    #[test]
    fn no_two_phases_are_named_the_same_thing() {
        // The account names the phase and nothing else does; two phases with one
        // name is a reader who cannot tell which device state a reset found.
        for (at, phase) in EVERY.iter().enumerate() {
            for other in &EVERY[at + 1..] {
                assert_ne!(phase.named(), other.named(), "{phase:?} and {other:?}");
            }
        }
    }
}
