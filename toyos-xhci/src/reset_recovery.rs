//! Bulk-Only Transport's Reset Recovery, as the commands an xHC owes before
//! the three requests the class defines.
//!
//! USB Mass Storage Class Bulk-Only Transport 1.0 §5.3.4 makes the recovery
//! three requests, in order: a Bulk-Only Mass Storage Reset (§3.1), a
//! ClearFeature(ENDPOINT_HALT) to the Bulk-In endpoint, and one to the
//! Bulk-Out. **The second and third are not conditional on a halt.** A
//! ClearFeature(ENDPOINT_HALT) reinitialises the device's data toggle whether
//! the endpoint was halted or not (USB 2.0 §9.4.5), so after it the device
//! expects the host to start both pipes at zero.
//!
//! What the host owes for that is two things per endpoint. First, taking it
//! off whatever it was running, to Stopped: Reset Endpoint from Halted (xHCI
//! 1.2 §4.6.8), Stop Endpoint from Running (§4.6.9), Set TR Dequeue Pointer
//! from Error (§4.6.10), nothing from Stopped. Second, zeroing its own toggle
//! or sequence number to match the device's, which for an endpoint that is not
//! Halted a Configure Endpoint with the Drop and Add flags set does (§4.6.8's
//! note, which defines it from Stopped): one such command re-creates both
//! endpoints Running on fresh rings ([`crate::configure`]).
//!
//! **An event outranks the Endpoint State field.** The field is the
//! controller's and may lag the endpoint: §4.8.3's note has its update
//! deferred past an error condition and past a command's completion, and tells
//! software to keep its own image, driven by events. [`Quiescing`] is that
//! image for the length of one quiesce: a transfer event's completion code is
//! where a pipe starts ([`event_state`]), a command that succeeded leaves it
//! Stopped, and the field is read only for a pipe no event has spoken for.
//!
//! **One command per look, and a Context State Error is an answer.** A
//! transfer the driver stopped waiting for can still error, which moves the
//! endpoint out of Running between the look and the Stop Endpoint chosen from
//! it (§4.6.9's note). The state the command was chosen from is then ruled
//! out for that pipe: a field that moved is believed, and one that still reads
//! the ruled-out state is a field that lags, so the plan goes on to where
//! §4.6.9's note says a Running endpoint goes by itself — Halted, then Error.
//! At most [`MOST_LOOKS`] looks.
//!
//! **Every command before the first request.** A request reaches the device
//! and a command does not, so the commands, which end every transfer on the
//! host side, all come first.
//!
//! **The device's own answer ends it, and nothing else does.** §3.1 has the
//! device ready for the next CBW once it answers the reset, and a device that
//! answers and is not is one no request can tell from one that is. So the
//! recovery closes with a TEST UNIT READY — a command with no data phase, which
//! moves nothing whatever phase the device takes it in — and has taken only
//! on a status carrying that command's tag ([`crate::bot::whose`]). A command
//! with a buffer is not sent to a device that has not given one.

use crate::recovery::{Command, EndpointState};

/// One of a device's two bulk endpoints.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pipe {
    In,
    Out,
}

impl Pipe {
    /// The pipe a transfer in this direction runs on.
    pub fn of(device_to_host: bool) -> Self {
        if device_to_host {
            Self::In
        } else {
            Self::Out
        }
    }

    fn index(self) -> usize {
        match self {
            Self::In => 0,
            Self::Out => 1,
        }
    }
}

/// Completion code 19 (xHCI 1.2 Table 6-90).
pub const CONTEXT_STATE_ERROR: u32 = 19;
const SUCCESS: u32 = 1;

/// The endpoint state a transfer event's completion code reports, for the
/// codes that report one: §4.8.3 lists Babble Detected (3), USB Transaction
/// Error (4), Stall Error (6) and Split Transaction Error (36) as Halt
/// conditions, and has a TRB Error (5) leave the endpoint in Error.
pub fn event_state(code: u32) -> Option<EndpointState> {
    match code {
        3 | 4 | 6 | 36 => Some(EndpointState::Halted),
        5 => Some(EndpointState::Error),
        _ => None,
    }
}

/// What one look at the pair asks of the driver.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Look {
    /// Issue this against this pipe and report its answer to
    /// [`Quiescing::answered`], then look again.
    Command(Command, Pipe),
    /// Both endpoints are Stopped: [`AFTER_QUIESCE`] is what is left.
    Stopped,
    /// The pair cannot be taken to Stopped, and no further command is owed.
    GaveUp(GaveUp),
}

/// Why a quiesce ended short of Stopped.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GaveUp {
    /// The field reads a state no command leaves; only a Configure Endpoint
    /// makes this endpoint again.
    NeedsConfigure(Pipe, EndpointState),
    /// The controller refused the command with a code that is not about the
    /// endpoint's state.
    Refused(Command, u32),
    /// The controller did not answer the command.
    Silent(Command),
    /// Every state a command is defined for has been refused for this pipe.
    Contradicted(Pipe),
    /// [`MOST_LOOKS`] were taken.
    OutOfLooks,
}

/// What an answer meant, for the driver's log; the plan has already acted on
/// it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Answered {
    Took,
    /// A Context State Error: the pipe was not in `from`, the state the
    /// command was chosen for.
    Moved { from: EndpointState },
    /// The next look gives up.
    Ended,
}

/// Looks a quiesce gets. Each pipe costs at most three commands — Stop
/// Endpoint from the Running its field reads, then Reset Endpoint and Set TR
/// Dequeue Pointer for the two states an error can have moved it to under a
/// field that never says so — and the seventh look finds both Stopped.
pub const MOST_LOOKS: u8 = 7;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Image {
    /// What events have said, which outranks the field.
    known: Option<EndpointState>,
    /// States a Context State Error has said the pipe is not in.
    not: [bool; 3],
}

impl Image {
    const BLANK: Self = Self { known: None, not: [false; 3] };

    fn slot(state: EndpointState) -> Option<usize> {
        match state {
            EndpointState::Running => Some(0),
            EndpointState::Halted => Some(1),
            EndpointState::Error => Some(2),
            _ => None,
        }
    }

    fn ruled_out(&self, state: EndpointState) -> bool {
        Self::slot(state).is_some_and(|at| self.not[at])
    }

    /// The state to choose a command from, given what the field reads.
    fn believed(&self, field: EndpointState) -> Option<EndpointState> {
        if matches!(field, EndpointState::Disabled | EndpointState::Unusable(_)) {
            return Some(field);
        }
        if let Some(known) = self.known {
            return Some(known);
        }
        [field, EndpointState::Halted, EndpointState::Error]
            .into_iter()
            .find(|state| !self.ruled_out(*state))
    }
}

/// One quiesce of a bulk pair, as the image of both endpoints and the looks
/// still left.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Quiescing {
    looks: u8,
    pipes: [Image; 2],
    issued: Option<(Command, Pipe, EndpointState)>,
    ended: Option<GaveUp>,
}

impl Quiescing {
    /// `broke` is the pipe and completion code of the transfer event that
    /// ended the round trip, where one did; a break with no event (silence, a
    /// malformed status block) starts from the fields alone.
    pub fn begin(broke: Option<(Pipe, u32)>) -> Self {
        let mut pipes = [Image::BLANK; 2];
        if let Some((pipe, code)) = broke {
            pipes[pipe.index()].known = event_state(code);
        }
        Self { looks: 0, pipes, issued: None, ended: None }
    }

    /// The next command that takes the pair towards Stopped, Bulk-In first.
    pub fn look(&mut self, in_field: EndpointState, out_field: EndpointState) -> Look {
        if let Some(why) = self.ended {
            return Look::GaveUp(why);
        }
        if self.looks == MOST_LOOKS {
            return self.give_up(GaveUp::OutOfLooks);
        }
        self.looks += 1;
        for (pipe, field) in [(Pipe::In, in_field), (Pipe::Out, out_field)] {
            let Some(state) = self.pipes[pipe.index()].believed(field) else {
                return self.give_up(GaveUp::Contradicted(pipe));
            };
            let cmd = match state {
                EndpointState::Halted => Command::ResetEndpoint,
                EndpointState::Running => Command::StopEndpoint,
                EndpointState::Error => Command::SetDequeue,
                EndpointState::Stopped => continue,
                EndpointState::Disabled | EndpointState::Unusable(_) => {
                    return self.give_up(GaveUp::NeedsConfigure(pipe, state))
                }
            };
            self.issued = Some((cmd, pipe, state));
            return Look::Command(cmd, pipe);
        }
        Look::Stopped
    }

    /// The controller's answer to the command the last look asked for, or
    /// `None` for a controller that gave none.
    pub fn answered(&mut self, code: Option<u32>) -> Answered {
        let (cmd, pipe, from) = self.issued.take().expect("an answer to a command no look asked for");
        let image = &mut self.pipes[pipe.index()];
        match code {
            Some(SUCCESS) => {
                image.known = Some(EndpointState::Stopped);
                Answered::Took
            }
            Some(CONTEXT_STATE_ERROR) => {
                image.known = None;
                if let Some(at) = Image::slot(from) {
                    image.not[at] = true;
                }
                Answered::Moved { from }
            }
            Some(code) => {
                self.ended = Some(GaveUp::Refused(cmd, code));
                Answered::Ended
            }
            None => {
                self.ended = Some(GaveUp::Silent(cmd));
                Answered::Ended
            }
        }
    }

    fn give_up(&mut self, why: GaveUp) -> Look {
        self.ended = Some(why);
        Look::GaveUp(why)
    }
}

/// One step of the recovery once both endpoints are Stopped.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    /// Configure Endpoint with both bulk endpoints dropped and added, on fresh
    /// rings.
    Reconfigure,
    /// The Bulk-Only Mass Storage Reset (BOT §3.1, §5.3.4 (a)).
    MassStorageReset,
    /// ClearFeature(ENDPOINT_HALT) on one pipe (§5.3.4 (b) and (c)).
    ClearHalt(Pipe),
}

/// What follows the quiesce, in the order it is taken: the one command left,
/// then the class's three requests.
pub const AFTER_QUIESCE: [Step; 4] = [
    Step::Reconfigure,
    Step::MassStorageReset,
    Step::ClearHalt(Pipe::In),
    Step::ClearHalt(Pipe::Out),
];

/// Who gives back the slot of a disk that is given up on while it is still
/// plugged in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SlotGoes {
    /// Back to the controller now, by Disable Slot.
    Back,
    /// Nowhere until the device leaves its port: the port keeps holding it and
    /// its teardown gives it back.
    WithTheUnplug,
}

/// Disable Slot is defined over endpoints that are Stopped, or Running with
/// nothing to run (xHCI 1.2 §4.6.4's note). A pair the quiesce could not take
/// to Stopped keeps its slot, inside a bind as much as after one.
pub fn slot_after_offline(pair_stopped: bool) -> SlotGoes {
    if pair_stopped {
        SlotGoes::Back
    } else {
        SlotGoes::WithTheUnplug
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECOVERABLE: [EndpointState; 4] = [
        EndpointState::Halted,
        EndpointState::Running,
        EndpointState::Stopped,
        EndpointState::Error,
    ];

    /// xHCI 1.2 §4.8.3's endpoint state machine for the three commands a
    /// quiesce issues: the state each leaves, or `None` for the Context State
    /// Error every other state answers with.
    fn controller(state: EndpointState, cmd: Command) -> Option<EndpointState> {
        match (cmd, state) {
            (Command::ResetEndpoint, EndpointState::Halted)
            | (Command::StopEndpoint, EndpointState::Running)
            | (Command::SetDequeue, EndpointState::Stopped | EndpointState::Error) => {
                Some(EndpointState::Stopped)
            }
            _ => None,
        }
    }

    /// A pair as the model holds it: where each endpoint is, what its field
    /// reads, and where an abandoned transfer's error moves it between the
    /// look that finds it Running and the command chosen from that.
    #[derive(Clone, Copy)]
    struct Pair {
        actual: [EndpointState; 2],
        field: [EndpointState; 2],
        moves_under: [Option<EndpointState>; 2],
        /// A field that lags is never written again: §4.8.3's note puts no
        /// bound on the deferral.
        lags: bool,
    }

    impl Pair {
        fn current(in_state: EndpointState, out_state: EndpointState) -> Self {
            let actual = [in_state, out_state];
            Self { actual, field: actual, moves_under: [None; 2], lags: false }
        }
    }

    /// What a walk came to: the looks taken to Stopped, and the commands the
    /// controller refused on the way.
    #[derive(Debug, PartialEq, Eq)]
    struct Walked {
        looks: u8,
        refused: u8,
    }

    /// Run `plan` against the model until it stops asking. `Err` is the give-up.
    fn walk(mut plan: Quiescing, mut pair: Pair) -> Result<Walked, GaveUp> {
        let (mut looks, mut refused) = (0, 0);
        loop {
            looks += 1;
            assert!(looks <= MOST_LOOKS + 1, "a quiesce that outlives its own bound");
            match plan.look(pair.field[0], pair.field[1]) {
                Look::Stopped => {
                    assert_eq!(pair.actual, [EndpointState::Stopped; 2], "called Stopped and is not");
                    return Ok(Walked { looks, refused });
                }
                Look::GaveUp(why) => return Err(why),
                Look::Command(cmd, pipe) => {
                    let at = pipe.index();
                    if pair.actual[at] == EndpointState::Running {
                        if let Some(to) = pair.moves_under[at].take() {
                            pair.actual[at] = to;
                        }
                    }
                    let code = match controller(pair.actual[at], cmd) {
                        Some(next) => {
                            pair.actual[at] = next;
                            SUCCESS
                        }
                        None => {
                            refused += 1;
                            CONTEXT_STATE_ERROR
                        }
                    };
                    if !pair.lags {
                        pair.field[at] = pair.actual[at];
                    }
                    plan.answered(Some(code));
                }
            }
        }
    }

    fn every_pair() -> impl Iterator<Item = (EndpointState, EndpointState)> {
        RECOVERABLE
            .into_iter()
            .flat_map(|a| RECOVERABLE.into_iter().map(move |b| (a, b)))
    }

    const MOVES: [Option<EndpointState>; 3] =
        [None, Some(EndpointState::Halted), Some(EndpointState::Error)];

    /// With a field that is current, every command a look yields is one
    /// §4.8.3 defines for the state that look saw, so a Context State Error is
    /// only ever the state having moved.
    #[test]
    fn every_command_is_defined_for_the_state_it_was_chosen_from() {
        for (a, b) in every_pair() {
            let walked = walk(Quiescing::begin(None), Pair::current(a, b)).expect("recoverable");
            assert_eq!(walked.refused, 0, "{a:?}/{b:?}");
        }
    }

    /// Whatever pair a break leaves, wherever an error moves either pipe under
    /// its look, and whether or not the field ever says so, the pair is
    /// Stopped inside the bound.
    #[test]
    fn every_pair_is_stopped_inside_the_bound_however_it_moves_and_however_the_field_lags() {
        for (a, b) in every_pair() {
            for in_moves in MOVES {
                for out_moves in MOVES {
                    for lags in [false, true] {
                        let pair = Pair {
                            moves_under: [in_moves, out_moves],
                            lags,
                            ..Pair::current(a, b)
                        };
                        let walked = walk(Quiescing::begin(None), pair);
                        assert!(
                            walked.is_ok(),
                            "{a:?}/{b:?} moving {in_moves:?}/{out_moves:?} lags={lags}: {walked:?}"
                        );
                    }
                }
            }
        }
    }

    /// The bound is the worst case and not a margin over it: both pipes
    /// Running, both moved to Error under their look, neither field saying so.
    #[test]
    fn the_bound_is_the_worst_case() {
        let mut worst = 0;
        for in_moves in MOVES {
            for out_moves in MOVES {
                for lags in [false, true] {
                    let pair = Pair {
                        moves_under: [in_moves, out_moves],
                        lags,
                        ..Pair::current(EndpointState::Running, EndpointState::Running)
                    };
                    worst = worst.max(walk(Quiescing::begin(None), pair).expect("recoverable").looks);
                }
            }
        }
        assert_eq!(worst, MOST_LOOKS);
    }

    /// §4.8.3's note, the case a plan that reads the field goes wrong on: the
    /// transfer event said Stall, Babble or Transaction Error, the endpoint is
    /// Halted, and the field still reads Running. The event's word is taken:
    /// Reset Endpoint first, no command refused, and the pair Stopped in three
    /// looks — where choosing from the field spends every look on a Stop
    /// Endpoint the controller refuses, and takes a healthy disk offline.
    #[test]
    fn a_halting_completion_code_outranks_a_field_that_still_reads_running() {
        for code in [3, 4, 6] {
            for pipe in [Pipe::In, Pipe::Out] {
                let mut pair = Pair::current(EndpointState::Running, EndpointState::Running);
                pair.actual[pipe.index()] = EndpointState::Halted;
                pair.lags = true;
                let mut plan = Quiescing::begin(Some((pipe, code)));
                let first = plan.look(pair.field[0], pair.field[1]);
                if pipe == Pipe::In {
                    assert_eq!(first, Look::Command(Command::ResetEndpoint, Pipe::In), "code {code}");
                }
                let walked = walk(Quiescing::begin(Some((pipe, code))), pair).expect("recoverable");
                assert_eq!(walked, Walked { looks: 3, refused: 0 }, "code {code} on {pipe:?}");
            }
        }
    }

    /// A command that succeeded is an event too: its pipe is Stopped whatever
    /// the field goes on reading, so it is never commanded twice.
    #[test]
    fn a_command_that_took_is_not_issued_again_over_a_field_that_lags() {
        let pair = Pair { lags: true, ..Pair::current(EndpointState::Halted, EndpointState::Running) };
        let walked = walk(Quiescing::begin(None), pair).expect("recoverable");
        assert_eq!(walked, Walked { looks: 3, refused: 0 });
    }

    /// A Context State Error is the state having moved, never the end of the
    /// quiesce: with a current field the next look reads where it went.
    #[test]
    fn a_context_state_error_is_followed_by_the_command_for_where_the_pipe_went() {
        let mut plan = Quiescing::begin(None);
        let (running, halted, stopped) =
            (EndpointState::Running, EndpointState::Halted, EndpointState::Stopped);
        assert_eq!(plan.look(running, stopped), Look::Command(Command::StopEndpoint, Pipe::In));
        assert_eq!(plan.answered(Some(CONTEXT_STATE_ERROR)), Answered::Moved { from: running });
        assert_eq!(plan.look(halted, stopped), Look::Command(Command::ResetEndpoint, Pipe::In));
        assert_eq!(plan.answered(Some(SUCCESS)), Answered::Took);
        assert_eq!(plan.look(halted, stopped), Look::Stopped);
    }

    /// Any other refusal, and a controller that does not answer, end it: the
    /// next look owes no command.
    #[test]
    fn a_refusal_that_is_not_about_state_and_a_silence_both_end_the_quiesce() {
        let running = EndpointState::Running;
        for (code, why) in [
            (Some(5), GaveUp::Refused(Command::StopEndpoint, 5)),
            (None, GaveUp::Silent(Command::StopEndpoint)),
        ] {
            let mut plan = Quiescing::begin(None);
            assert_eq!(plan.look(running, running), Look::Command(Command::StopEndpoint, Pipe::In));
            assert_eq!(plan.answered(code), Answered::Ended);
            assert_eq!(plan.look(running, running), Look::GaveUp(why));
            assert_eq!(plan.look(running, running), Look::GaveUp(why), "and stays ended");
        }
    }

    /// A controller that refuses every command its states define is not asked
    /// a fourth time.
    #[test]
    fn a_pipe_refused_from_every_state_is_given_up_on_by_name() {
        let mut plan = Quiescing::begin(None);
        let running = EndpointState::Running;
        for cmd in [Command::StopEndpoint, Command::ResetEndpoint, Command::SetDequeue] {
            assert_eq!(plan.look(running, running), Look::Command(cmd, Pipe::In));
            plan.answered(Some(CONTEXT_STATE_ERROR));
        }
        assert_eq!(plan.look(running, running), Look::GaveUp(GaveUp::Contradicted(Pipe::In)));
    }

    /// A pair with nothing to move costs one look and no command.
    #[test]
    fn a_stopped_pair_is_asked_nothing() {
        let stopped = EndpointState::Stopped;
        assert_eq!(Quiescing::begin(None).look(stopped, stopped), Look::Stopped);
    }

    /// Bulk-In first, so the order the record shows is the order taken.
    #[test]
    fn the_in_pipe_is_quiesced_before_the_out_pipe() {
        assert_eq!(
            Quiescing::begin(None).look(EndpointState::Halted, EndpointState::Running),
            Look::Command(Command::ResetEndpoint, Pipe::In)
        );
        assert_eq!(
            Quiescing::begin(None).look(EndpointState::Stopped, EndpointState::Running),
            Look::Command(Command::StopEndpoint, Pipe::Out)
        );
    }

    /// An endpoint in a state no command leaves is refused by name, naming the
    /// pipe; the other pipe's state does not rescue it, and neither does an
    /// event.
    #[test]
    fn a_disabled_or_reserved_endpoint_refuses_the_whole_recovery() {
        let stopped = EndpointState::Stopped;
        for bad in [EndpointState::Disabled, EndpointState::Unusable(5), EndpointState::Unusable(7)]
        {
            for broke in [None, Some((Pipe::In, 6)), Some((Pipe::Out, 6))] {
                assert_eq!(
                    Quiescing::begin(broke).look(bad, stopped),
                    Look::GaveUp(GaveUp::NeedsConfigure(Pipe::In, bad)),
                    "{bad:?} in, {broke:?}"
                );
            }
            assert_eq!(
                Quiescing::begin(None).look(stopped, bad),
                Look::GaveUp(GaveUp::NeedsConfigure(Pipe::Out, bad)),
                "{bad:?} out"
            );
        }
    }

    /// Table 6-90's numbers against §4.8.3's two lists; a code that reports no
    /// state leaves the pipe to its field.
    #[test]
    fn only_a_halt_condition_or_a_trb_error_speaks_for_the_endpoint() {
        for code in 0..=255 {
            let want = match code {
                3 | 4 | 6 | 36 => Some(EndpointState::Halted),
                5 => Some(EndpointState::Error),
                _ => None,
            };
            assert_eq!(event_state(code), want, "code {code}");
        }
        // Short Packet is a transfer that ended early, not an endpoint that
        // stopped.
        let mut plan = Quiescing::begin(Some((Pipe::In, 13)));
        assert_eq!(
            plan.look(EndpointState::Running, EndpointState::Stopped),
            Look::Command(Command::StopEndpoint, Pipe::In)
        );
    }

    /// §4.6.4's note, for the bound disk and the one still inside its bind
    /// alike: the slot goes back only over a Stopped pair.
    #[test]
    fn a_slot_goes_back_only_over_a_stopped_pair() {
        assert_eq!(slot_after_offline(true), SlotGoes::Back);
        assert_eq!(slot_after_offline(false), SlotGoes::WithTheUnplug);
    }
}
