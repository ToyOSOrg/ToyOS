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
//! endpoints Running on fresh rings.
//!
//! **One command per look, and the look after it decides the next.** The
//! Endpoint State field is the controller's and moves without the driver: a
//! transfer the driver stopped waiting for can still error, which turns
//! Running into Halted between the look and the Stop Endpoint chosen from it,
//! and the controller answers that command with a Context State Error
//! (§4.8.3). A sequence planned from one look goes wrong there on a race and
//! not on a device fact, so [`quiesce`] is asked again after every answer, at
//! most [`MOST_LOOKS`] times.
//!
//! **Every command before the first request.** A request reaches the device
//! and a command does not, so the commands, which end every transfer on the
//! host side, all come first.

use crate::recovery::{Command, EndpointState, NeedsConfigure};

/// One of a device's two bulk endpoints.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pipe {
    In,
    Out,
}

/// What one look at the pair's two states asks of the driver.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Quiesce {
    /// Issue this against this pipe, then look again whatever it answered.
    Command(Command, Pipe),
    /// Both endpoints are Stopped: [`AFTER_QUIESCE`] is what is left.
    Stopped,
}

/// Looks a quiesce gets. Each pipe costs at most two commands — the one its
/// state asked for, and the one that answers the state an error moved it to
/// under that look — and the fifth look is the one that finds both Stopped.
pub const MOST_LOOKS: u8 = 5;

/// The next command that takes the pair towards Stopped, Bulk-In first, or a
/// refusal naming the endpoint no command is defined for.
pub fn quiesce(
    in_state: EndpointState,
    out_state: EndpointState,
) -> Result<Quiesce, (Pipe, NeedsConfigure)> {
    for (pipe, state) in [(Pipe::In, in_state), (Pipe::Out, out_state)] {
        let cmd = match state {
            EndpointState::Halted => Command::ResetEndpoint,
            EndpointState::Running => Command::StopEndpoint,
            EndpointState::Error => Command::SetDequeue,
            EndpointState::Stopped => continue,
            EndpointState::Disabled | EndpointState::Unusable(_) => {
                return Err((pipe, NeedsConfigure(state)))
            }
        };
        return Ok(Quiesce::Command(cmd, pipe));
    }
    Ok(Quiesce::Stopped)
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

    /// Run a quiesce against the model. `errors_under` names the pipes whose
    /// abandoned transfer errors between the look that finds them Running and
    /// the command chosen from it. Answers the looks taken, or `None` if the
    /// bound ran out first.
    fn walk(
        mut in_state: EndpointState,
        mut out_state: EndpointState,
        mut errors_under: [bool; 2],
    ) -> Option<u8> {
        for look in 1..=MOST_LOOKS {
            match quiesce(in_state, out_state).expect("recoverable") {
                Quiesce::Stopped => return Some(look),
                Quiesce::Command(cmd, pipe) => {
                    let (state, errors) = match pipe {
                        Pipe::In => (&mut in_state, &mut errors_under[0]),
                        Pipe::Out => (&mut out_state, &mut errors_under[1]),
                    };
                    if *state == EndpointState::Running && core::mem::take(errors) {
                        *state = EndpointState::Halted;
                    }
                    if let Some(next) = controller(*state, cmd) {
                        *state = next;
                    }
                }
            }
        }
        None
    }

    fn every_pair() -> impl Iterator<Item = (EndpointState, EndpointState)> {
        RECOVERABLE
            .into_iter()
            .flat_map(|a| RECOVERABLE.into_iter().map(move |b| (a, b)))
    }

    /// Every command a look yields is one §4.8.3 defines for the state that
    /// look saw, so a Context State Error is only ever the state having moved.
    #[test]
    fn every_command_is_defined_for_the_state_it_was_chosen_from() {
        for (a, b) in every_pair() {
            if let Quiesce::Command(cmd, pipe) = quiesce(a, b).expect("recoverable") {
                let state = if pipe == Pipe::In { a } else { b };
                assert!(controller(state, cmd).is_some(), "{cmd:?} against {state:?}");
            }
        }
    }

    /// Whatever pair a break leaves, and whichever pipes error under their
    /// look, the pair is Stopped inside the bound.
    #[test]
    fn every_pair_is_stopped_inside_the_bound_whichever_pipes_error_under_the_look() {
        for (a, b) in every_pair() {
            for errors_under in [[false, false], [true, false], [false, true], [true, true]] {
                assert!(
                    walk(a, b, errors_under).is_some(),
                    "{a:?}/{b:?} with {errors_under:?} is not Stopped in {MOST_LOOKS} looks"
                );
            }
        }
    }

    /// The bound is the worst case and not a margin over it: both pipes
    /// Running and both erroring under their look spends every look.
    #[test]
    fn the_bound_is_the_worst_case() {
        let worst = walk(EndpointState::Running, EndpointState::Running, [true, true]);
        assert_eq!(worst, Some(MOST_LOOKS));
    }

    /// A pair with nothing to move costs one look and no command.
    #[test]
    fn a_stopped_pair_is_asked_nothing() {
        assert_eq!(
            quiesce(EndpointState::Stopped, EndpointState::Stopped),
            Ok(Quiesce::Stopped)
        );
    }

    /// Bulk-In first, so the order the record shows is the order taken.
    #[test]
    fn the_in_pipe_is_quiesced_before_the_out_pipe() {
        assert_eq!(
            quiesce(EndpointState::Halted, EndpointState::Running),
            Ok(Quiesce::Command(Command::ResetEndpoint, Pipe::In))
        );
        assert_eq!(
            quiesce(EndpointState::Stopped, EndpointState::Running),
            Ok(Quiesce::Command(Command::StopEndpoint, Pipe::Out))
        );
    }

    /// An endpoint in a state no command leaves is refused by name, naming the
    /// pipe, and the other pipe's state does not rescue it.
    #[test]
    fn a_disabled_or_reserved_endpoint_refuses_the_whole_recovery() {
        for bad in [EndpointState::Disabled, EndpointState::Unusable(5), EndpointState::Unusable(7)]
        {
            assert_eq!(
                quiesce(bad, EndpointState::Stopped),
                Err((Pipe::In, NeedsConfigure(bad))),
                "{bad:?} in"
            );
            assert_eq!(
                quiesce(EndpointState::Stopped, bad),
                Err((Pipe::Out, NeedsConfigure(bad))),
                "{bad:?} out"
            );
        }
    }
}
