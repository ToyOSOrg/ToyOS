//! The machine's stop, as the supervisor decides it: what a stop request, a
//! terminate, a cancel and the sessions' ends each make it do. Pure.
//!
//! **A stop asks every login session to quit with a terminate, and its mode
//! says what then.** A [`Mode::Wait`] stop waits for the sessions to end, as
//! events, for as long as they take; only a terminate, a later request by a
//! holder of `power`, kills what stays. A [`Mode::Terminate`] stop kills it as
//! soon as the quits are asked. **Nothing here reads a clock**, so no deadline
//! forces a stop. A cancel ends a pending stop that has not begun killing;
//! the quits it asked stay asked.

use toyos::power::{Mode, Refused, Request, Stop};

/// What the supervisor does next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    /// Ask every session to quit with a terminate.
    AskSessions,
    /// Kill every session's owner, and with it what stays.
    KillSessions,
    /// Tell every session the stop is cancelled.
    CancelSessions,
    /// Tell every watcher the stop is cancelled.
    Cancelled,
    /// Tell every watcher what the stop waits for now.
    Report,
    /// Every session has ended: flush the log and ask the kernel.
    Stop(Stop),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Idle,
    Pending { stop: Stop, killing: bool },
}

/// The machine's stop, if one is pending.
#[derive(Debug)]
pub struct Stops {
    state: State,
}

impl Default for Stops {
    fn default() -> Self {
        Self { state: State::Idle }
    }
}

impl Stops {
    /// The stop pending, if one is.
    pub fn pending(&self) -> Option<Stop> {
        match self.state {
            State::Idle => None,
            State::Pending { stop, .. } => Some(stop),
        }
    }

    /// A stop asked, with `open` sessions still open. **Answered at once**: the
    /// stop taken or joined, or why it was refused.
    pub fn ask(&mut self, asked: Request, open: usize) -> (Result<(), Refused>, Vec<Act>) {
        match self.state {
            State::Idle => {
                self.state = State::Pending { stop: asked.stop, killing: false };
                let mut acts = vec![Act::AskSessions];
                if asked.mode == Mode::Terminate {
                    acts.extend(self.kill());
                }
                acts.extend(self.sessions(open));
                (Ok(()), acts)
            }
            State::Pending { stop, .. } if stop != asked.stop => (Err(Refused::AnotherStopPending(stop)), Vec::new()),
            State::Pending { .. } => match asked.mode {
                Mode::Wait => (Ok(()), Vec::new()),
                Mode::Terminate => (Ok(()), self.kill()),
            },
        }
    }

    /// Force the pending stop.
    pub fn terminate(&mut self) -> (Result<(), Refused>, Vec<Act>) {
        match self.state {
            State::Idle => (Err(Refused::NoStopPending), Vec::new()),
            State::Pending { .. } => (Ok(()), self.kill()),
        }
    }

    /// End the pending stop; the machine keeps running.
    pub fn cancel(&mut self) -> (Result<(), Refused>, Vec<Act>) {
        match self.state {
            State::Idle => (Err(Refused::NoStopPending), Vec::new()),
            State::Pending { killing: true, .. } => (Err(Refused::TooLate), Vec::new()),
            State::Pending { killing: false, .. } => {
                self.state = State::Idle;
                (Ok(()), vec![Act::CancelSessions, Act::Cancelled])
            }
        }
    }

    /// The sessions changed, `open` of them left: a report while a stop
    /// waits, and the stop once none is.
    pub fn sessions(&mut self, open: usize) -> Vec<Act> {
        match self.state {
            State::Idle => Vec::new(),
            State::Pending { stop, .. } if open == 0 => vec![Act::Stop(stop)],
            State::Pending { .. } => vec![Act::Report],
        }
    }

    /// The kernel refused the stop: the machine runs on, and none is pending.
    pub fn refused(&mut self) {
        self.state = State::Idle;
    }

    fn kill(&mut self) -> Vec<Act> {
        match &mut self.state {
            State::Pending { killing: killing @ false, .. } => {
                *killing = true;
                vec![Act::KillSessions]
            }
            _ => Vec::new(),
        }
    }
}

/// One watcher of a stop: the frame it is owed, while its connection cannot
/// take one. **Only the latest list and the outcome are kept**, so a watcher
/// that never reads blocks nobody and is never sent a list after its outcome.
#[derive(Debug, Default)]
pub struct Owed {
    frame: Option<(u32, Vec<u8>, bool)>,
}

impl Owed {
    /// `frame`, an outcome where `outcome`, sent by `send` unless it would
    /// block; what is owed is the latest, or the outcome once there is one.
    pub fn post(&mut self, msg: u32, payload: Vec<u8>, outcome: bool, send: impl FnOnce(u32, &[u8]) -> bool) {
        if self.frame.as_ref().is_some_and(|(_, _, was)| *was) {
            return;
        }
        match self.frame.is_none() && send(msg, &payload) {
            true => {}
            false => self.frame = Some((msg, payload, outcome)),
        }
    }

    /// Send what is owed, now its connection may take it.
    pub fn flush(&mut self, send: impl FnOnce(u32, &[u8]) -> bool) {
        if let Some((msg, payload, _)) = &self.frame {
            if send(*msg, payload) {
                self.frame = None;
            }
        }
    }

    pub fn owes(&self) -> bool {
        self.frame.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WAIT: Request = Request { stop: Stop::Shutdown, mode: Mode::Wait };
    const TERMINATE: Request = Request { stop: Stop::Shutdown, mode: Mode::Terminate };

    /// A waiting stop asks the sessions, is answered at once, and stops only
    /// once the last has ended, however long it takes: it never kills.
    #[test]
    fn a_waiting_stop_asks_and_waits_and_never_kills() {
        let mut stops = Stops::default();
        assert_eq!(stops.ask(WAIT, 2), (Ok(()), vec![Act::AskSessions, Act::Report]));
        for _ in 0..1000 {
            assert_eq!(stops.sessions(1), vec![Act::Report], "a waiting stop gave up on its sessions");
        }
        assert_eq!(stops.sessions(0), vec![Act::Stop(Stop::Shutdown)]);
    }

    /// A terminating stop kills what stays as soon as it has asked, and stops
    /// once the owners have ended.
    #[test]
    fn a_terminating_stop_kills_at_once() {
        let mut stops = Stops::default();
        assert_eq!(stops.ask(TERMINATE, 1), (Ok(()), vec![Act::AskSessions, Act::KillSessions, Act::Report]));
        assert_eq!(stops.sessions(0), vec![Act::Stop(Stop::Shutdown)]);
        assert_eq!(Stops::default().ask(TERMINATE, 0).1, vec![Act::AskSessions, Act::KillSessions, Act::Stop(Stop::Shutdown)]);
    }

    /// A terminate forces a waiting stop; a second stop of its kind joins it,
    /// and with terminate forces it; another kind is refused, never replaces it.
    #[test]
    fn a_second_request_joins_forces_or_is_refused() {
        let mut stops = Stops::default();
        let _ = stops.ask(WAIT, 1);
        assert_eq!(stops.ask(WAIT, 1), (Ok(()), Vec::new()));
        let reboot = Request { stop: Stop::Reboot, mode: Mode::Wait };
        assert_eq!(stops.ask(reboot, 1), (Err(Refused::AnotherStopPending(Stop::Shutdown)), Vec::new()));
        assert_eq!(stops.pending(), Some(Stop::Shutdown));
        assert_eq!(stops.ask(TERMINATE, 1), (Ok(()), vec![Act::KillSessions]));
        assert_eq!(stops.terminate(), (Ok(()), Vec::new()), "a second force kills nothing again");
        let mut waiting = Stops::default();
        let _ = waiting.ask(WAIT, 1);
        assert_eq!(waiting.terminate(), (Ok(()), vec![Act::KillSessions]));
    }

    /// A cancel keeps the machine running and tells everyone; once killing
    /// has begun it is too late; with nothing pending, both are refused.
    #[test]
    fn a_cancel_keeps_the_machine_running_until_killing_begins() {
        let mut stops = Stops::default();
        assert_eq!(stops.cancel().0, Err(Refused::NoStopPending));
        assert_eq!(stops.terminate().0, Err(Refused::NoStopPending));
        let _ = stops.ask(WAIT, 1);
        assert_eq!(stops.cancel(), (Ok(()), vec![Act::CancelSessions, Act::Cancelled]));
        assert_eq!(stops.pending(), None);
        assert_eq!(stops.sessions(0), Vec::new(), "a cancelled stop stopped the machine");
        let _ = stops.ask(WAIT, 1);
        let _ = stops.terminate();
        assert_eq!(stops.cancel().0, Err(Refused::TooLate));
    }

    /// A watcher that never reads is owed the latest list, then the outcome,
    /// which no later list replaces, and gets it once it reads.
    #[test]
    fn a_slow_watcher_still_receives_the_outcome() {
        let mut owed = Owed::default();
        owed.post(5, b"a".to_vec(), false, |_, _| false);
        owed.post(5, b"b".to_vec(), false, |_, _| panic!("a second frame was sent past one owed"));
        owed.post(6, Vec::new(), true, |_, _| panic!("the outcome was sent past a list owed"));
        owed.post(5, b"c".to_vec(), false, |_, _| panic!("a list was sent after the outcome"));
        let mut sent = Vec::new();
        owed.flush(|msg, payload| {
            sent.push((msg, payload.to_vec()));
            true
        });
        assert_eq!(sent, vec![(6, Vec::new())]);
        assert!(!owed.owes());
    }
}
