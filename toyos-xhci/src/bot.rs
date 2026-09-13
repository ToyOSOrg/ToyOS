//! Where one Bulk-Only Transport command stands, and what a reset owes the
//! device that is inside it.
//!
//! **A reset is not a way to end a command.** USB Mass Storage Class Bulk-Only
//! Transport 1.0 §5.1 makes every command three transfers — the 31-byte CBW
//! out, an optional data phase, the 13-byte CSW in — and §6.7.2/§6.7.3 leave a
//! device that has taken a CBW waiting for whichever of them has not arrived.
//! A host that stops between two phases has told the device nothing. The
//! class's own way out is Reset Recovery (§5.3.4): a class request and two
//! CLEAR_FEATURE(HALT)s, all three of them control transfers on a live device,
//! which is not a sequence a wedged kernel can issue. What such a kernel *can*
//! do is finish the command it opened — the data the CBW promised, then the
//! CSW — after which the device is back where §5.1 leaves it between commands.
//!
//! The phases are here and their effects are the driver's, because the reader
//! is the reset path: it runs where no lock may be taken, so it reads the phase
//! out of an atomic rather than out of the driver, and every decision it then
//! makes is this module's.

/// Where one Bulk-Only round trip stands, published by the driver as it walks
/// the three phases.
///
/// **Five and not three**, though two pairs owe the same acts: which of a pair
/// a wedged machine stopped in is the difference between a device the
/// controller was still serving and one nothing had asked yet, and the reset's
/// account is the only place that is ever said.
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

/// What a reset must put on the wire before it takes a port down.
///
/// [`Self::data`] and [`Self::status`] are about transfers the *host* has not
/// queued. A transfer already queued is the controller's to finish and
/// re-queueing it would move the same bytes twice — which for an out data phase
/// is a second write of the block, so the distinction is not a nicety.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Owed {
    /// The data phase the CBW promised has not been queued.
    pub data: bool,
    /// The data endpoint's doorbell must be rung.
    ///
    /// **Queued is not running.** The driver publishes its phase between the
    /// enqueue and the doorbell, so that a reset can see the ring a transfer
    /// went on — which means a machine stopped in that window leaves a TRB the
    /// controller was never told about, and a reset that re-rang only the status
    /// endpoint would wait out a data phase nothing is moving. A doorbell for a
    /// transfer already running is a hint the controller may ignore (xHCI 1.2
    /// §4.7), so this is rung whenever the data phase is still the device's to
    /// receive — queued by the driver, or by [`Self::data`] just now.
    pub ring_data: bool,
    /// The CSW's transfer has not been queued. Until the device has sent one it
    /// takes no further command (BOT §5.3), so this is what a reset owes even
    /// where the data moved.
    pub status: bool,
}

/// What finishing the command in `phase` takes, or `None` where no command is
/// open and the reset owes the device nothing.
///
/// `data_len` is the CBW's own dCBWDataTransferLength (§5.1): zero is a command
/// with no data phase, which owes only its status.
pub fn owed(phase: Phase, data_len: u32) -> Option<Owed> {
    if !phase.open() {
        return None;
    }
    let has_data = data_len > 0;
    Some(Owed {
        data: has_data && matches!(phase, Phase::Command | Phase::DataOwed),
        ring_data: has_data
            && matches!(phase, Phase::Command | Phase::DataOwed | Phase::Data),
        status: phase != Phase::Status,
    })
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
    fn a_closed_command_owes_the_device_nothing() {
        for len in [0, 13, 4096, u32::MAX] {
            assert_eq!(owed(Phase::Closed, len), None, "{len} B");
        }
    }

    #[test]
    fn every_open_phase_but_the_last_owes_a_status() {
        for phase in EVERY.iter().copied().filter(|p| p.open()) {
            let owed = owed(phase, 4096).expect("open");
            assert_eq!(
                owed.status,
                phase != Phase::Status,
                "{phase:?} disagrees about the CSW it owes"
            );
        }
    }

    #[test]
    fn a_data_phase_already_queued_is_never_queued_again() {
        // The negative control on the whole module: re-queueing an out data
        // phase writes the block twice, which is worse than the reset it is
        // avoiding.
        for phase in [Phase::Data, Phase::StatusOwed, Phase::Status] {
            assert!(!owed(phase, 4096).expect("open").data, "{phase:?} re-queued its data");
        }
    }

    #[test]
    fn a_data_phase_the_device_is_still_owed_is_always_rung_for() {
        // Queued is not running: the phase is published between the enqueue and
        // the doorbell, so `Data` can mean a TRB the controller never saw.
        for phase in [Phase::Command, Phase::DataOwed, Phase::Data] {
            assert!(
                owed(phase, 4096).expect("open").ring_data,
                "{phase:?} left a data phase nothing would move"
            );
        }
        // And past it the device has the bytes; a doorbell there would be for a
        // transfer that is over.
        for phase in [Phase::StatusOwed, Phase::Status] {
            assert!(!owed(phase, 4096).expect("open").ring_data, "{phase:?} rang for moved data");
        }
    }

    #[test]
    fn a_command_with_no_data_phase_owes_none() {
        for phase in EVERY.iter().copied().filter(|p| p.open()) {
            let owed = owed(phase, 0).expect("open");
            assert!(!owed.data, "{phase:?} owed data it never promised");
            assert!(!owed.ring_data, "{phase:?} rang for a data phase it never promised");
        }
    }

    #[test]
    fn the_two_phases_that_precede_a_data_transfer_owe_it() {
        for phase in [Phase::Command, Phase::DataOwed] {
            assert_eq!(
                owed(phase, 4096).expect("open"),
                Owed { data: true, ring_data: true, status: true },
                "{phase:?}"
            );
        }
    }
}
