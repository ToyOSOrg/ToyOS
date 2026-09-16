//! Bulk-Only Transport's Reset Recovery, as the commands an xHC owes before
//! the three requests the class defines.
//!
//! USB Mass Storage Class Bulk-Only Transport 1.0 §5.3.4 makes the recovery
//! three requests, in order: a Bulk-Only Mass Storage Reset (§3.1), a
//! ClearFeature(ENDPOINT_HALT) to the Bulk-In endpoint, and one to the
//! Bulk-Out. **The second and third are not conditional on a halt.** A
//! ClearFeature(ENDPOINT_HALT) reinitialises the device's data toggle
//! (USB 2.0 §9.4.5) or sequence number (USB 3.2 §9.4.5) whether the endpoint
//! was halted or not, so after it the device expects the host to start both
//! pipes at zero — and a host that cleared only the pipe its controller had
//! halted leaves the other pipe's two ends disagreeing, which is a transfer
//! the device answers with no valid handshake.
//!
//! What the host owes for that is two things per endpoint. First, taking it
//! out of the state it broke in: Reset Endpoint from Halted (xHCI 1.2 §4.6.8),
//! Stop Endpoint from Running (§4.6.9), nothing from Stopped. Second, zeroing
//! its own toggle or sequence number to match the device's, which for an
//! endpoint that is not Halted only a Configure Endpoint with the Drop and Add
//! flags set does (§4.8.1): one such command re-creates both endpoints Running
//! on fresh rings, so no Set TR Dequeue Pointer is owed either.
//!
//! **Every command before the first request.** A request reaches the device
//! and a command does not. A device still holding the transfer the host
//! stopped waiting for answers it when next asked, and that answer landing on
//! a state machine the class reset has already rewound undoes the reset — so
//! the commands, which end every transfer on the host side, all come first.

use crate::recovery::{Command, EndpointState, NeedsConfigure};

/// One of a device's two bulk endpoints.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pipe {
    In,
    Out,
}

/// One step of the recovery, in the order [`ResetRecovery::steps`] hands them
/// out.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    /// A command against one endpoint, which the controller answers and the
    /// device never sees. Never [`Command::SetDequeue`]: the reconfigure below
    /// places both rings.
    Command(Command, Pipe),
    /// Configure Endpoint with both bulk endpoints dropped and added, on fresh
    /// rings, which is what zeroes the host's toggle or sequence number for an
    /// endpoint that was not Halted (xHCI 1.2 §4.8.1).
    Reconfigure,
    /// The Bulk-Only Mass Storage Reset (BOT §3.1, §5.3.4 (a)).
    MassStorageReset,
    /// ClearFeature(ENDPOINT_HALT) on one pipe (§5.3.4 (b) and (c)).
    ClearHalt(Pipe),
}

/// The most steps a plan holds: one command per pipe, the reconfigure, the
/// class reset and one clear per pipe.
const MOST: usize = 6;

/// One device's Reset Recovery, as the steps still to take.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ResetRecovery {
    steps: [Step; MOST],
    len: u8,
}

impl ResetRecovery {
    /// The recovery a device whose bulk pair is in these two states needs, or
    /// a refusal naming the endpoint no sequence of commands takes back.
    pub fn plan(
        in_state: EndpointState,
        out_state: EndpointState,
    ) -> Result<Self, (Pipe, NeedsConfigure)> {
        let quiesce_in = quiesce(in_state).map_err(|why| (Pipe::In, why))?;
        let quiesce_out = quiesce(out_state).map_err(|why| (Pipe::Out, why))?;
        let mut steps = [Step::Reconfigure; MOST];
        let mut len = 0usize;
        let mut push = |step: Step| {
            steps[len] = step;
            len += 1;
        };
        if let Some(cmd) = quiesce_in {
            push(Step::Command(cmd, Pipe::In));
        }
        if let Some(cmd) = quiesce_out {
            push(Step::Command(cmd, Pipe::Out));
        }
        push(Step::Reconfigure);
        push(Step::MassStorageReset);
        push(Step::ClearHalt(Pipe::In));
        push(Step::ClearHalt(Pipe::Out));
        Ok(Self { steps, len: len as u8 })
    }

    /// The steps, in the order they are taken.
    pub fn steps(&self) -> &[Step] {
        &self.steps[..usize::from(self.len)]
    }
}

/// The command that takes one endpoint out of the state it broke in, or none
/// where it is already out of the way.
fn quiesce(state: EndpointState) -> Result<Option<Command>, NeedsConfigure> {
    match state {
        EndpointState::Halted => Ok(Some(Command::ResetEndpoint)),
        EndpointState::Running => Ok(Some(Command::StopEndpoint)),
        // Stop Endpoint against a Stopped endpoint is a Context State Error
        // (§4.6.9), and the reconfigure needs nothing more from it.
        EndpointState::Stopped => Ok(None),
        state @ (EndpointState::Disabled | EndpointState::Unusable(_)) => {
            Err(NeedsConfigure(state))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECOVERABLE: [EndpointState; 3] =
        [EndpointState::Halted, EndpointState::Running, EndpointState::Stopped];

    fn every_pair() -> impl Iterator<Item = (EndpointState, EndpointState)> {
        RECOVERABLE
            .into_iter()
            .flat_map(|a| RECOVERABLE.into_iter().map(move |b| (a, b)))
    }

    /// The T14's own case: the status phase babbled, so the controller halted
    /// the Bulk-In and left the Bulk-Out Running.
    #[test]
    fn a_halted_in_pipe_beside_a_running_out_pipe() {
        let plan = ResetRecovery::plan(EndpointState::Halted, EndpointState::Running)
            .expect("recoverable");
        assert_eq!(
            plan.steps(),
            [
                Step::Command(Command::ResetEndpoint, Pipe::In),
                Step::Command(Command::StopEndpoint, Pipe::Out),
                Step::Reconfigure,
                Step::MassStorageReset,
                Step::ClearHalt(Pipe::In),
                Step::ClearHalt(Pipe::Out),
            ]
        );
    }

    /// **The three requests are the class's and are unconditional**: whatever
    /// state either endpoint was found in, the plan ends with the reset, then
    /// the Bulk-In clear, then the Bulk-Out clear, in §5.3.4's order.
    #[test]
    fn every_plan_ends_with_the_classs_three_requests_in_order() {
        for (a, b) in every_pair() {
            let plan = ResetRecovery::plan(a, b).expect("recoverable");
            let steps = plan.steps();
            assert_eq!(
                &steps[steps.len() - 3..],
                [Step::MassStorageReset, Step::ClearHalt(Pipe::In), Step::ClearHalt(Pipe::Out)],
                "{a:?}/{b:?}: {steps:?}"
            );
        }
    }

    /// A ClearFeature(ENDPOINT_HALT) zeroes the device's toggle or sequence
    /// number on both pipes, so the host has to zero its own for both — which
    /// is one reconfigure, and it comes after every quiesce and before every
    /// request.
    #[test]
    fn every_plan_reconfigures_both_pipes_exactly_once_between_the_halves() {
        for (a, b) in every_pair() {
            let plan = ResetRecovery::plan(a, b).expect("recoverable");
            let steps = plan.steps();
            let at = steps.iter().position(|s| *s == Step::Reconfigure).expect("a reconfigure");
            assert_eq!(steps.iter().filter(|s| **s == Step::Reconfigure).count(), 1, "{steps:?}");
            assert!(
                steps[..at].iter().all(|s| matches!(s, Step::Command(..))),
                "{a:?}/{b:?}: a request before the reconfigure: {steps:?}"
            );
            assert!(
                steps[at + 1..].iter().all(|s| !matches!(s, Step::Command(..))),
                "{a:?}/{b:?}: a command after the reconfigure: {steps:?}"
            );
        }
    }

    /// **The bus is reached only after every command.** A command ends a
    /// transfer on the host side; a request is what the device answers, and a
    /// device still answering an ended transfer would undo the reset.
    #[test]
    fn the_bus_is_reached_only_after_every_command() {
        for (a, b) in every_pair() {
            let steps = ResetRecovery::plan(a, b).expect("recoverable");
            let steps = steps.steps();
            let first_request = steps
                .iter()
                .position(|s| matches!(s, Step::MassStorageReset | Step::ClearHalt(_)))
                .expect("a request");
            let last_command = steps
                .iter()
                .rposition(|s| matches!(s, Step::Command(..) | Step::Reconfigure))
                .expect("a command");
            assert!(last_command < first_request, "{a:?}/{b:?}: {steps:?}");
        }
    }

    /// Which command takes an endpoint out of its state is the state's alone:
    /// Reset Endpoint is defined only for Halted (§4.6.8) and Stop Endpoint
    /// only for Running (§4.6.9), and a Stopped endpoint gets neither.
    #[test]
    fn each_pipe_gets_the_one_command_its_state_permits() {
        for (a, b) in every_pair() {
            let plan = ResetRecovery::plan(a, b).expect("recoverable");
            for (pipe, state) in [(Pipe::In, a), (Pipe::Out, b)] {
                let mut commands = plan.steps().iter().filter_map(|s| match s {
                    Step::Command(cmd, on) if *on == pipe => Some(*cmd),
                    _ => None,
                });
                let want = match state {
                    EndpointState::Halted => Some(Command::ResetEndpoint),
                    EndpointState::Running => Some(Command::StopEndpoint),
                    EndpointState::Stopped => None,
                    _ => unreachable!(),
                };
                assert_eq!(commands.next(), want, "{pipe:?} in {state:?}");
                assert_eq!(commands.next(), None, "{pipe:?} in {state:?} got a second command");
            }
        }
    }

    /// The reconfigure places both rings, so a Set TR Dequeue Pointer would be
    /// a second placement of a ring the controller was just handed.
    #[test]
    fn no_plan_sets_a_dequeue_pointer() {
        for (a, b) in every_pair() {
            let plan = ResetRecovery::plan(a, b).expect("recoverable");
            assert!(
                !plan.steps().iter().any(|s| matches!(s, Step::Command(Command::SetDequeue, _))),
                "{a:?}/{b:?}: {:?}",
                plan.steps()
            );
        }
    }

    /// An endpoint in a state no command leaves is refused by name, naming the
    /// pipe, and the other pipe's state does not rescue it.
    #[test]
    fn a_disabled_or_reserved_endpoint_refuses_the_whole_recovery() {
        for bad in [EndpointState::Disabled, EndpointState::Unusable(4), EndpointState::Unusable(7)]
        {
            for good in RECOVERABLE {
                assert_eq!(
                    ResetRecovery::plan(bad, good),
                    Err((Pipe::In, NeedsConfigure(bad))),
                    "{bad:?} in"
                );
                assert_eq!(
                    ResetRecovery::plan(good, bad),
                    Err((Pipe::Out, NeedsConfigure(bad))),
                    "{bad:?} out"
                );
            }
        }
    }

    /// The array is sized for the longest plan and no plan runs off it.
    #[test]
    fn the_longest_plan_fills_the_array() {
        let plan = ResetRecovery::plan(EndpointState::Halted, EndpointState::Halted)
            .expect("recoverable");
        assert_eq!(plan.steps().len(), MOST);
        let plan = ResetRecovery::plan(EndpointState::Stopped, EndpointState::Stopped)
            .expect("recoverable");
        assert_eq!(plan.steps().len(), MOST - 2);
    }
}
