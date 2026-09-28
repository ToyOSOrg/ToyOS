//! Where one Bulk-Only Transport command stands.
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
//!
//! [`RoundTrip`] is the round trip as a machine the driver answers: which
//! transfer is owed, what each completion means, the one legal stall retry, and
//! whether the CSW is believed. The driver queues and waits; nothing here
//! touches a ring, so a driver that waits in place and one that gives the CPU
//! back between transfers drive the same order.

use crate::job::{CC_SHORT_PACKET, CC_STALL, CC_SUCCESS};
use crate::ladder::{self, Left};
use crate::reset_recovery::Pipe;
use crate::scsi::Cdb;

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

pub const CBW_LEN: usize = 31;
pub const CSW_LEN: usize = 13;
const CBW_SIGNATURE: u32 = 0x4342_5355;
const CSW_SIGNATURE: u32 = 0x5342_5355;

/// The Command Block Wrapper (§5.1) carrying `cdb` under `tag`, for a data
/// phase of `data_len` bytes, to LUN 0: this driver binds one logical unit.
pub fn cbw(tag: u32, data_len: u32, cdb: &Cdb) -> [u8; CBW_LEN] {
    let mut out = [0u8; CBW_LEN];
    out[0..4].copy_from_slice(&CBW_SIGNATURE.to_le_bytes());
    out[4..8].copy_from_slice(&tag.to_le_bytes());
    out[8..12].copy_from_slice(&data_len.to_le_bytes());
    out[12] = if cdb.data_in() { 0x80 } else { 0x00 };
    let bytes = cdb.bytes();
    out[14] = bytes.len() as u8;
    out[15..15 + bytes.len()].copy_from_slice(bytes);
    out
}

/// How one round trip ended, where the device answered in step.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Bot {
    /// CSW status 0; `delivered` is the smaller of what the controller moved
    /// and what the device says it did not leave unmoved — else stale data
    /// from an earlier LBA leaks. Never more than the transfer.
    Done { delivered: u32 },
    /// CSW status 1: the device understood and refused. Sense data says why.
    Failed,
}

/// Why a round trip could not be completed; what happened decides which
/// recovery is legal. `W` is the driver's reason for a silence.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Broke<W> {
    /// The controller reported this completion code for the phase, on this
    /// pipe.
    Code { phase: Phase, code: u32, pipe: Pipe },
    /// Nothing came back for the phase.
    Silence { phase: Phase, why: W },
    /// The port reads empty: the device is no longer on the bus.
    Gone { phase: Phase },
    /// A CBW or CSW moved the wrong byte count; both are fixed length, so
    /// short is not a short transfer.
    Short { phase: Phase, moved: u32, wanted: u32 },
    /// The endpoint stalled and its restart did not take.
    Stall { phase: Phase },
    /// CSW status 2, which leaves both endpoints Running.
    PhaseError,
    /// A CSW status §5.2 reserves, which is no meaningful CSW (§6.3.2).
    Reserved { status: u8 },
    Csw { what: &'static str, got: u32, want: u32, status: u8, residue: u32 },
    /// More bytes claimed unmoved than the transfer had; believing it would
    /// underflow the byte count every caller uses.
    Residue { unmoved: u32, of: u32 },
}

impl<W> Broke<W> {
    /// Where the break left the device ([`ladder::left`]); `data_out` is
    /// whether the command that broke sends data.
    pub fn left(&self, data_out: bool) -> Left {
        let phase = match self {
            Self::Code { phase, .. }
            | Self::Silence { phase, .. }
            | Self::Gone { phase }
            | Self::Stall { phase }
            | Self::Short { phase, .. } => *phase,
            Self::PhaseError | Self::Reserved { .. } | Self::Csw { .. } | Self::Residue { .. } => {
                Phase::Status
            }
        };
        ladder::left(phase, data_out)
    }

    /// The transfer event that ended the round trip, where one did: what a
    /// quiesce takes ahead of the pipe's Endpoint State field.
    pub fn event(&self) -> Option<(Pipe, u32)> {
        match self {
            Self::Code { code, pipe, .. } => Some((*pipe, *code)),
            _ => None,
        }
    }
}

/// What the round trip asks of the driver next.
///
/// Before [`Act::Data`] the device holds a CBW and nothing is queued for it
/// ([`Phase::DataOwed`]), and before [`Act::Status`] it holds a CSW nothing has
/// asked for ([`Phase::StatusOwed`]); the driver publishes both.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Act {
    /// The CBW, [`CBW_LEN`] bytes on the Bulk-Out.
    Command,
    /// The data phase, the whole of its region, on this pipe.
    Data(Pipe),
    /// Into a zeroed buffer: the CSW, [`CSW_LEN`] bytes on the Bulk-In.
    Status,
    /// The pipe stalled: back to running, and once it is, `then` is the phase
    /// published over its rebuilt ring.
    Restart { pipe: Pipe, then: Phase },
}

/// How the driver's last [`Act`] ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Answer<W> {
    /// The transfer's completion code, and the bytes it left unmoved.
    Moved { code: u32, residue: u32 },
    Silent(W),
    Gone,
    /// Whether the restart took.
    Restarted(bool),
}

/// Where a round trip stands.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum At {
    Command,
    Data,
    /// The data phase stalled with this many bytes unmoved.
    DataRestart { unmoved: u32 },
    Status { retried: bool },
    StatusRestart,
}

/// One Bulk-Only round trip: command block out, data, status in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RoundTrip {
    at: At,
    tag: u32,
    data_len: u32,
    data_in: bool,
    /// What the controller says reached the buffer, checked against the CSW's
    /// residue.
    moved: u32,
}

/// Where a round trip goes after an answer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Next<W> {
    Act(RoundTrip, Act),
    /// All thirteen bytes of a CSW arrived: [`CswDue::judge`] them.
    Csw(CswDue),
    Broke(Broke<W>),
}

impl RoundTrip {
    /// The round trip of the CBW sent under `tag` for `cdb`, with a data
    /// phase of `data_len` bytes.
    pub fn begin(tag: u32, data_len: u32, cdb: &Cdb) -> (Self, Act) {
        let trip = Self { at: At::Command, tag, data_len, data_in: cdb.data_in(), moved: 0 };
        (trip, Act::Command)
    }

    fn status(self, retried: bool) -> (Self, Act) {
        (Self { at: At::Status { retried }, ..self }, Act::Status)
    }

    /// The last act ended with `answer`.
    pub fn answered<W>(self, answer: Answer<W>) -> Next<W> {
        let data_pipe = Pipe::of(self.data_in);
        match (self.at, answer) {
            (At::Command, answer) => match framed(Phase::Command, Pipe::Out, CBW_LEN as u32, answer) {
                Err(broke) => Next::Broke(broke),
                Ok(()) if self.data_len > 0 => Next::Act(Self { at: At::Data, ..self }, Act::Data(data_pipe)),
                Ok(()) => {
                    let (trip, act) = self.status(false);
                    Next::Act(trip, act)
                }
            },
            (At::Data, Answer::Moved { code: CC_SUCCESS | CC_SHORT_PACKET, residue }) => {
                let (trip, act) = Self { moved: self.data_len.saturating_sub(residue), ..self }.status(false);
                Next::Act(trip, act)
            }
            // A stalled data phase is ordinary — an unsupported command, a read
            // past the end — and the status that follows its recovery turns it
            // into a clean refusal (§6.7.2).
            (At::Data, Answer::Moved { code: CC_STALL, residue }) => Next::Act(
                Self { at: At::DataRestart { unmoved: residue }, ..self },
                Act::Restart { pipe: data_pipe, then: Phase::Data },
            ),
            (At::Data, Answer::Moved { code, .. }) => {
                Next::Broke(Broke::Code { phase: Phase::Data, code, pipe: data_pipe })
            }
            (At::Data, Answer::Silent(why)) => Next::Broke(Broke::Silence { phase: Phase::Data, why }),
            (At::Data, Answer::Gone) => Next::Broke(Broke::Gone { phase: Phase::Data }),
            (At::DataRestart { unmoved }, Answer::Restarted(true)) => {
                let (trip, act) = Self { moved: self.data_len.saturating_sub(unmoved), ..self }.status(false);
                Next::Act(trip, act)
            }
            (At::DataRestart { .. }, Answer::Restarted(false)) => Next::Broke(Broke::Stall { phase: Phase::Data }),
            // The class's one legal retry: a device may STALL the status
            // phase once (§5.3.3, Figure 2).
            (At::Status { retried: false }, Answer::Moved { code: CC_STALL, .. }) => Next::Act(
                Self { at: At::StatusRestart, ..self },
                Act::Restart { pipe: Pipe::In, then: Phase::StatusOwed },
            ),
            (At::Status { .. }, answer) => match framed(Phase::Status, Pipe::In, CSW_LEN as u32, answer) {
                Ok(()) => Next::Csw(CswDue { tag: self.tag, data_len: self.data_len, moved: self.moved }),
                Err(broke) => Next::Broke(broke),
            },
            (At::StatusRestart, Answer::Restarted(true)) => {
                let (trip, act) = self.status(true);
                Next::Act(trip, act)
            }
            (At::StatusRestart, Answer::Restarted(false)) => Next::Broke(Broke::Stall { phase: Phase::Status }),
            (at, _) => panic!("a round trip at {at:?} was answered for an act it did not ask for"),
        }
    }
}

/// A CBW or CSW leg, which the device must take or give in full. Short Packet
/// is how the controller reports a sub-maximum-packet transfer — a 13-byte
/// CSW on a 512-byte endpoint — so only the residue says it all moved.
fn framed<W>(phase: Phase, pipe: Pipe, len: u32, answer: Answer<W>) -> Result<(), Broke<W>> {
    match answer {
        Answer::Moved { code: CC_SUCCESS | CC_SHORT_PACKET, residue: 0 } => Ok(()),
        Answer::Moved { code: CC_SUCCESS | CC_SHORT_PACKET, residue } => {
            Err(Broke::Short { phase, moved: len.saturating_sub(residue), wanted: len })
        }
        Answer::Moved { code, .. } => Err(Broke::Code { phase, code, pipe }),
        Answer::Silent(why) => Err(Broke::Silence { phase, why }),
        Answer::Gone => Err(Broke::Gone { phase }),
        Answer::Restarted(_) => panic!("a {phase} transfer was answered as a restart"),
    }
}

/// A CSW that arrived whole, owed its judgement.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CswDue {
    tag: u32,
    data_len: u32,
    moved: u32,
}

impl CswDue {
    /// Every field checked (§6.3), never believed. Status 2 is meaningful
    /// whatever the residue; 0 and 1 only with one no larger than the transfer.
    pub fn judge<W>(self, csw: &[u8; CSW_LEN]) -> Result<Bot, Broke<W>> {
        let word = |at: usize| u32::from_le_bytes([csw[at], csw[at + 1], csw[at + 2], csw[at + 3]]);
        let (signature, tag, residue, status) = (word(0), word(4), word(8), csw[12]);
        if signature != CSW_SIGNATURE {
            return Err(Broke::Csw { what: "signature", got: signature, want: CSW_SIGNATURE, status, residue });
        }
        // A mismatched tag would attribute one command's status to another — a
        // write reporting the read before it as success.
        if tag != self.tag {
            return Err(Broke::Csw { what: "tag", got: tag, want: self.tag, status, residue });
        }
        match status {
            2 => Err(Broke::PhaseError),
            0 | 1 if residue > self.data_len => Err(Broke::Residue { unmoved: residue, of: self.data_len }),
            // Neither account is trusted alone: a caller may read only what
            // both the device and the controller say arrived.
            0 => Ok(Bot::Done { delivered: self.moved.min(self.data_len - residue) }),
            1 => Ok(Bot::Failed),
            status => Err(Broke::Reserved { status }),
        }
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
    fn no_two_phases_are_named_the_same_thing() {
        // The account names the phase and nothing else does; two phases with one
        // name is a reader who cannot tell which device state a reset found.
        for (at, phase) in EVERY.iter().enumerate() {
            for other in &EVERY[at + 1..] {
                assert_ne!(phase.named(), other.named(), "{phase:?} and {other:?}");
            }
        }
    }

    /// The driver's reason for a silence, which the machine only carries.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    struct Why;

    const TAG: u32 = 0x0102_0304;
    const WHOLE: Answer<Why> = Answer::Moved { code: CC_SUCCESS, residue: 0 };

    fn read(sectors: u16) -> Cdb {
        Cdb::transfer(false, 0, sectors)
    }

    fn write(sectors: u16) -> Cdb {
        Cdb::transfer(true, 0, sectors)
    }

    /// A CSW as §5.2 lays it out.
    fn csw(tag: u32, residue: u32, status: u8) -> [u8; CSW_LEN] {
        let mut out = [0u8; CSW_LEN];
        out[0..4].copy_from_slice(b"USBS");
        out[4..8].copy_from_slice(&tag.to_le_bytes());
        out[8..12].copy_from_slice(&residue.to_le_bytes());
        out[12] = status;
        out
    }

    /// Sized past the longest route, so one that grew runs off it.
    const LONGEST: usize = 8;

    struct Walk {
        acts: [Option<Act>; LONGEST],
        end: Result<Bot, Broke<Why>>,
    }

    impl Walk {
        fn acts(&self) -> impl Iterator<Item = Act> + '_ {
            self.acts.iter().map_while(|a| *a)
        }

        fn count(&self, act: Act) -> usize {
            self.acts().filter(|a| *a == act).count()
        }
    }

    /// A round trip driven to its end: each act answered by `device`, and the
    /// CSW, once one arrives whole, is `status`.
    fn walk(cdb: Cdb, data_len: u32, mut device: impl FnMut(Act) -> Answer<Why>, status: [u8; CSW_LEN]) -> Walk {
        let (mut trip, first) = RoundTrip::begin(TAG, data_len, &cdb);
        let mut acts = [None; LONGEST];
        let mut act = first;
        for slot in &mut acts {
            *slot = Some(act);
            match trip.answered(device(act)) {
                Next::Act(next, then) => (trip, act) = (next, then),
                Next::Csw(due) => return Walk { acts, end: due.judge(&status) },
                Next::Broke(broke) => return Walk { acts, end: Err(broke) },
            }
        }
        panic!("a round trip that does not end: {acts:?}");
    }

    /// §5.1's layout, byte for byte: "USBC", the tag and the length little
    /// endian, the direction in bit 7 of the flags, LUN 0, and the CDB's own
    /// length before it.
    #[test]
    fn a_cbw_is_laid_out_as_the_class_defines_it() {
        let got = cbw(TAG, 0x1000, &read(8));
        let want: [u8; CBW_LEN] = [
            b'U', b'S', b'B', b'C', 0x04, 0x03, 0x02, 0x01, 0x00, 0x10, 0x00, 0x00, 0x80, 0x00, 10,
            0x28, 0, 0, 0, 0, 0, 0, 0, 8, 0, 0, 0, 0, 0, 0, 0,
        ];
        assert_eq!(got, want);
        assert_eq!(cbw(TAG, 0x1000, &write(8))[12], 0x00, "Data-Out");
        let tur = cbw(TAG, 0, &Cdb::TEST_UNIT_READY);
        assert_eq!((tur[12], tur[14]), (0x00, 6));
        assert!(tur[15..].iter().all(|b| *b == 0));
    }

    #[test]
    fn a_command_with_no_data_is_its_command_block_and_its_status() {
        let walk = walk(Cdb::TEST_UNIT_READY, 0, |_| WHOLE, csw(TAG, 0, 0));
        assert!(walk.acts().eq([Act::Command, Act::Status]));
        assert_eq!(walk.end, Ok(Bot::Done { delivered: 0 }));
    }

    #[test]
    fn a_data_phase_runs_on_the_pipe_its_command_names() {
        let walk_in = walk(read(1), 512, |_| WHOLE, csw(TAG, 0, 0));
        assert!(walk_in.acts().eq([Act::Command, Act::Data(Pipe::In), Act::Status]));
        assert_eq!(walk_in.end, Ok(Bot::Done { delivered: 512 }));
        let walk_out = walk(write(1), 512, |_| WHOLE, csw(TAG, 0, 0));
        assert!(walk_out.acts().eq([Act::Command, Act::Data(Pipe::Out), Act::Status]));
    }

    /// Neither account alone: what the controller moved, and what the device
    /// says it did not.
    #[test]
    fn what_is_delivered_is_what_both_the_controller_and_the_device_say_arrived() {
        let short = |act| match act {
            Act::Data(_) => Answer::Moved { code: CC_SHORT_PACKET, residue: 100 },
            _ => WHOLE,
        };
        assert_eq!(walk(read(1), 512, short, csw(TAG, 0, 0)).end, Ok(Bot::Done { delivered: 412 }));
        assert_eq!(walk(read(1), 512, |_| WHOLE, csw(TAG, 200, 0)).end, Ok(Bot::Done { delivered: 312 }));
        assert_eq!(walk(read(1), 512, short, csw(TAG, 50, 0)).end, Ok(Bot::Done { delivered: 412 }));
        let overrun = |act| match act {
            Act::Data(_) => Answer::Moved { code: CC_SHORT_PACKET, residue: 600 },
            _ => WHOLE,
        };
        assert_eq!(walk(read(1), 512, overrun, csw(TAG, 0, 0)).end, Ok(Bot::Done { delivered: 0 }));
    }

    /// §6.7.2: the device STALLs a data phase it will not finish, and the
    /// status after the pipe's recovery says why.
    #[test]
    fn a_stalled_data_phase_is_restarted_and_its_status_read() {
        let stalls = |act| match act {
            Act::Data(_) => Answer::Moved { code: CC_STALL, residue: 512 },
            Act::Restart { .. } => Answer::Restarted(true),
            _ => WHOLE,
        };
        let walk = walk(read(1), 512, stalls, csw(TAG, 512, 1));
        assert!(walk.acts().eq([
            Act::Command,
            Act::Data(Pipe::In),
            Act::Restart { pipe: Pipe::In, then: Phase::Data },
            Act::Status,
        ]));
        assert_eq!(walk.end, Ok(Bot::Failed));
    }

    /// A stalled data phase's residue is the controller's account, and a
    /// status after the restart is the device's: neither alone.
    #[test]
    fn a_stalled_data_phase_delivers_what_both_the_controller_and_the_device_say_arrived() {
        let stalls_leaving = |unmoved| {
            move |act| match act {
                Act::Data(_) => Answer::Moved { code: CC_STALL, residue: unmoved },
                Act::Restart { .. } => Answer::Restarted(true),
                _ => WHOLE,
            }
        };
        assert_eq!(walk(read(1), 512, stalls_leaving(100), csw(TAG, 0, 0)).end, Ok(Bot::Done { delivered: 412 }));
        assert_eq!(walk(read(1), 512, stalls_leaving(600), csw(TAG, 0, 0)).end, Ok(Bot::Done { delivered: 0 }));
    }

    /// §5.3.3's one status retry is the status phase's, whatever the data
    /// phase before it needed.
    #[test]
    fn a_status_stalled_after_a_data_stall_is_still_asked_for_again() {
        let mut status_stalls = 1;
        let walk = walk(read(1), 512, |act| match act {
            Act::Data(_) => Answer::Moved { code: CC_STALL, residue: 512 },
            Act::Status if status_stalls > 0 => {
                status_stalls -= 1;
                Answer::Moved { code: CC_STALL, residue: CSW_LEN as u32 }
            }
            Act::Restart { .. } => Answer::Restarted(true),
            _ => WHOLE,
        }, csw(TAG, 512, 1));
        assert!(walk.acts().eq([
            Act::Command,
            Act::Data(Pipe::In),
            Act::Restart { pipe: Pipe::In, then: Phase::Data },
            Act::Status,
            Act::Restart { pipe: Pipe::In, then: Phase::StatusOwed },
            Act::Status,
        ]));
        assert_eq!(walk.end, Ok(Bot::Failed));
    }

    #[test]
    fn a_data_phase_whose_pipe_will_not_restart_breaks_as_a_stall() {
        let stuck = |act| match act {
            Act::Data(_) => Answer::Moved { code: CC_STALL, residue: 0 },
            Act::Restart { .. } => Answer::Restarted(false),
            _ => WHOLE,
        };
        let walk = walk(write(1), 512, stuck, csw(TAG, 0, 0));
        assert_eq!(walk.end, Err(Broke::Stall { phase: Phase::Data }));
        assert_eq!(walk.count(Act::Status), 0, "no status is asked of a pipe that did not restart");
    }

    /// §5.3.3: a STALLed CSW is asked for once more, and only once.
    #[test]
    fn a_stalled_status_is_asked_for_again_once() {
        let mut stalls = 1;
        let once = walk(Cdb::TEST_UNIT_READY, 0, |act| match act {
            Act::Status if stalls > 0 => {
                stalls -= 1;
                Answer::Moved { code: CC_STALL, residue: CSW_LEN as u32 }
            }
            Act::Restart { .. } => Answer::Restarted(true),
            _ => WHOLE,
        }, csw(TAG, 0, 0));
        assert!(once.acts().eq([
            Act::Command,
            Act::Status,
            Act::Restart { pipe: Pipe::In, then: Phase::StatusOwed },
            Act::Status,
        ]));
        assert_eq!(once.end, Ok(Bot::Done { delivered: 0 }));

        let always = walk(Cdb::TEST_UNIT_READY, 0, |act| match act {
            Act::Status => Answer::Moved { code: CC_STALL, residue: CSW_LEN as u32 },
            Act::Restart { .. } => Answer::Restarted(true),
            _ => WHOLE,
        }, csw(TAG, 0, 0));
        assert_eq!(always.count(Act::Status), 2);
        assert_eq!(always.end, Err(Broke::Code { phase: Phase::Status, code: CC_STALL, pipe: Pipe::In }));

        let stuck = walk(Cdb::TEST_UNIT_READY, 0, |act| match act {
            Act::Status => Answer::Moved { code: CC_STALL, residue: CSW_LEN as u32 },
            Act::Restart { .. } => Answer::Restarted(false),
            _ => WHOLE,
        }, csw(TAG, 0, 0));
        assert_eq!(stuck.end, Err(Broke::Stall { phase: Phase::Status }));
    }

    /// A CBW or CSW is fixed length, so a short one is a break and not a
    /// short transfer — and a Short Packet completion that moved it all is not.
    #[test]
    fn a_command_or_status_block_that_moved_short_breaks() {
        let short_cbw = walk(read(1), 512, |act| match act {
            Act::Command => Answer::Moved { code: CC_SUCCESS, residue: 1 },
            _ => WHOLE,
        }, csw(TAG, 0, 0));
        assert_eq!(short_cbw.end, Err(Broke::Short { phase: Phase::Command, moved: 30, wanted: 31 }));
        let short_csw = walk(read(1), 512, |act| match act {
            Act::Status => Answer::Moved { code: CC_SHORT_PACKET, residue: 4 },
            _ => WHOLE,
        }, csw(TAG, 0, 0));
        assert_eq!(short_csw.end, Err(Broke::Short { phase: Phase::Status, moved: 9, wanted: 13 }));
        let whole_csw = walk(read(1), 512, |act| match act {
            Act::Status => Answer::Moved { code: CC_SHORT_PACKET, residue: 0 },
            _ => WHOLE,
        }, csw(TAG, 0, 0));
        assert_eq!(whole_csw.end, Ok(Bot::Done { delivered: 512 }));
    }

    /// Every leg's silence, disconnect and error is a break in that leg, on
    /// the pipe it ran on, and nothing after it is asked.
    #[test]
    fn a_leg_that_fails_breaks_the_round_trip_where_it_failed() {
        for (leg, phase, pipe) in [
            (Act::Command, Phase::Command, Pipe::Out),
            (Act::Data(Pipe::Out), Phase::Data, Pipe::Out),
            (Act::Status, Phase::Status, Pipe::In),
        ] {
            let failing = |answer: Answer<Why>| move |act| if act == leg { answer } else { WHOLE };
            let end = |answer| walk(write(1), 512, failing(answer), csw(TAG, 0, 0));
            assert_eq!(end(Answer::Silent(Why)).end, Err(Broke::Silence { phase, why: Why }));
            assert_eq!(end(Answer::Gone).end, Err(Broke::Gone { phase }));
            let babble = end(Answer::Moved { code: 3, residue: 0 });
            assert_eq!(babble.end, Err(Broke::Code { phase, code: 3, pipe }));
            assert_eq!(babble.acts().last(), Some(leg));
        }
    }

    /// §6.3.1: valid means the signature and the tag. §6.3.2: meaningful means
    /// status 0 or 1 with a residue no larger than the transfer, or status 2
    /// whatever the residue; §5.2 reserves every other status.
    #[test]
    fn a_csw_is_believed_only_when_it_is_valid_and_meaningful() {
        let judged = |csw: [u8; CSW_LEN]| walk(read(1), 512, |_| WHOLE, csw).end;
        let mut unsigned = csw(TAG, 0, 0);
        unsigned[0] = b'X';
        assert!(matches!(judged(unsigned), Err(Broke::Csw { what: "signature", .. })));
        assert_eq!(
            judged(csw(TAG + 1, 7, 1)),
            Err(Broke::Csw { what: "tag", got: TAG + 1, want: TAG, status: 1, residue: 7 })
        );
        assert_eq!(judged(csw(TAG, 513, 0)), Err(Broke::Residue { unmoved: 513, of: 512 }));
        assert_eq!(judged(csw(TAG, 513, 1)), Err(Broke::Residue { unmoved: 513, of: 512 }));
        assert_eq!(judged(csw(TAG, 512, 0)), Ok(Bot::Done { delivered: 0 }));
        assert_eq!(judged(csw(TAG, 0, 1)), Ok(Bot::Failed));
        assert_eq!(judged(csw(TAG, 0, 2)), Err(Broke::PhaseError));
        assert_eq!(judged(csw(TAG, 513, 2)), Err(Broke::PhaseError));
        for status in 3..=u8::MAX {
            assert_eq!(judged(csw(TAG, 0, status)), Err(Broke::Reserved { status }), "status {status}");
        }
    }

    /// A break before a Data-Out phase is done leaves its device owed the
    /// data; a break in the status, or of a command that sends nothing, does
    /// not. Only a transfer event says where a pipe stands.
    #[test]
    fn a_break_says_where_it_left_the_device_and_which_event_ended_it() {
        let code = Broke::<Why>::Code { phase: Phase::Data, code: 4, pipe: Pipe::Out };
        assert_eq!(code.left(true), Left::OwedDataOut);
        assert_eq!(code.left(false), Left::Elsewhere);
        assert_eq!(code.event(), Some((Pipe::Out, 4)));
        let gone = Broke::<Why>::Gone { phase: Phase::Command };
        assert_eq!(gone.left(true), Left::OwedDataOut);
        assert_eq!(gone.event(), None);
        for status in [
            Broke::<Why>::PhaseError,
            Broke::Reserved { status: 3 },
            Broke::Residue { unmoved: 2, of: 1 },
            Broke::Csw { what: "tag", got: 0, want: 1, status: 0, residue: 0 },
            Broke::Silence { phase: Phase::Status, why: Why },
            Broke::Stall { phase: Phase::Status },
        ] {
            assert_eq!(status.left(true), Left::Elsewhere, "{status:?}");
            assert_eq!(status.event(), None, "{status:?}");
        }
    }

    #[test]
    #[should_panic(expected = "did not ask for")]
    fn a_restart_answered_with_a_transfer_is_a_driver_bug() {
        let (trip, _) = RoundTrip::begin(TAG, 512, &read(1));
        let Next::Act(trip, Act::Data(_)) = trip.answered::<Why>(WHOLE) else { panic!("no data act") };
        let Next::Act(trip, Act::Restart { .. }) = trip.answered::<Why>(Answer::Moved { code: CC_STALL, residue: 0 })
        else {
            panic!("no restart")
        };
        let _ = trip.answered::<Why>(WHOLE);
    }
}
