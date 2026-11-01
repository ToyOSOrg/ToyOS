//! The machine's stop, as the supervisor decides it: what a stop request, a
//! terminate, a cancel and the sessions' ends each make it do. Pure.
//!
//! **A [`Mode::Wait`] stop asks every login session to quit with a
//! terminate** and waits for them to end, as events, for as long as they
//! take; only a terminate, a later request by a holder of `power`, kills what
//! stays. A [`Mode::Terminate`] stop kills the sessions at once. **Nothing
//! here reads a clock**, so no deadline forces a stop. A cancel ends a pending
//! stop that has not begun killing; the quits it asked stay asked.

use toyos::power::{Mode, Refused, Request, Stop, Ticket};

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
    Pending { stop: Stop, ticket: Ticket, killing: bool },
}

/// The machine's stop, if one is pending.
#[derive(Debug)]
pub struct Stops {
    state: State,
    /// The next stop's ticket: never reused in a boot.
    next: Ticket,
}

impl Default for Stops {
    fn default() -> Self {
        Self { state: State::Idle, next: 1 }
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

    /// A stop asked, with `open` sessions still open. **Answered at once**:
    /// the ticket of the stop taken or joined, or why it was refused.
    pub fn ask(&mut self, asked: Request, open: usize) -> (Result<Ticket, Refused>, Vec<Act>) {
        match self.state {
            State::Idle => {
                let ticket = self.next;
                self.next += 1;
                self.state = State::Pending { stop: asked.stop, ticket, killing: false };
                let mut acts = match asked.mode {
                    Mode::Wait => vec![Act::AskSessions],
                    Mode::Terminate => self.kill(),
                };
                acts.extend(self.sessions(open));
                (Ok(ticket), acts)
            }
            State::Pending { stop, .. } if stop != asked.stop => (Err(Refused::AnotherStopPending(stop)), Vec::new()),
            State::Pending { ticket, .. } => match asked.mode {
                Mode::Wait => (Ok(ticket), Vec::new()),
                Mode::Terminate => (Ok(ticket), self.kill()),
            },
        }
    }

    /// Force the pending stop, or, where `of` names one, only that stop.
    pub fn terminate(&mut self, of: Option<Ticket>) -> (Result<(), Refused>, Vec<Act>) {
        match self.state {
            State::Pending { ticket, .. } if of.is_none_or(|of| of == ticket) => (Ok(()), self.kill()),
            _ => (Err(Refused::NoStopPending), Vec::new()),
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

/// A frame owed to one watcher while its connection cannot take it.
#[derive(Debug)]
struct Frame {
    msg: u32,
    payload: Vec<u8>,
    outcome: bool,
}

/// One watcher of one stop: the frame it is owed, while its connection cannot
/// take one. **Only the latest list and the outcome are kept**, so a watcher
/// that never reads blocks nobody; **once its outcome is sent it is done**,
/// and nothing after, of this stop or a later one, is sent to it.
#[derive(Debug, Default)]
pub struct Owed {
    frame: Option<Frame>,
    done: bool,
}

impl Owed {
    /// A frame, an outcome where `outcome`, sent by `send` unless it would
    /// block; what is owed is the latest, or the outcome once there is one.
    pub fn post(&mut self, msg: u32, payload: Vec<u8>, outcome: bool, send: impl FnOnce(u32, &[u8]) -> bool) {
        if self.done || self.frame.as_ref().is_some_and(|owed| owed.outcome) {
            return;
        }
        match self.frame.is_none() && send(msg, &payload) {
            true => self.done = outcome,
            false => self.frame = Some(Frame { msg, payload, outcome }),
        }
    }

    /// Send what is owed, now its connection may take it.
    pub fn flush(&mut self, send: impl FnOnce(u32, &[u8]) -> bool) {
        if let Some(owed) = &self.frame {
            if send(owed.msg, &owed.payload) {
                self.done = owed.outcome;
                self.frame = None;
            }
        }
    }

    pub fn owes(&self) -> bool {
        self.frame.is_some()
    }

    /// Its outcome is sent: the watcher is let go.
    pub fn done(&self) -> bool {
        self.done
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
        assert_eq!(stops.ask(WAIT, 2), (Ok(1), vec![Act::AskSessions, Act::Report]));
        for _ in 0..1000 {
            assert_eq!(stops.sessions(1), vec![Act::Report], "a waiting stop gave up on its sessions");
        }
        assert_eq!(stops.sessions(0), vec![Act::Stop(Stop::Shutdown)]);
    }

    /// A terminating stop kills the sessions at once, asking nothing a kill
    /// would not let them answer, and stops once the owners have ended.
    #[test]
    fn a_terminating_stop_kills_at_once() {
        let mut stops = Stops::default();
        assert_eq!(stops.ask(TERMINATE, 1), (Ok(1), vec![Act::KillSessions, Act::Report]));
        assert_eq!(stops.sessions(0), vec![Act::Stop(Stop::Shutdown)]);
        assert_eq!(Stops::default().ask(TERMINATE, 0).1, vec![Act::KillSessions, Act::Stop(Stop::Shutdown)]);
    }

    /// A terminate forces a waiting stop; a second stop of its kind joins it,
    /// and with terminate forces it; another kind is refused, never replaces it.
    #[test]
    fn a_second_request_joins_forces_or_is_refused() {
        let mut stops = Stops::default();
        let _ = stops.ask(WAIT, 1);
        assert_eq!(stops.ask(WAIT, 1), (Ok(1), Vec::new()), "a joiner was not given the stop's ticket");
        let reboot = Request { stop: Stop::Reboot, mode: Mode::Wait };
        assert_eq!(stops.ask(reboot, 1), (Err(Refused::AnotherStopPending(Stop::Shutdown)), Vec::new()));
        assert_eq!(stops.pending(), Some(Stop::Shutdown));
        assert_eq!(stops.ask(TERMINATE, 1), (Ok(1), vec![Act::KillSessions]));
        assert_eq!(stops.terminate(None), (Ok(()), Vec::new()), "a second force kills nothing again");
        let mut waiting = Stops::default();
        let _ = waiting.ask(WAIT, 1);
        assert_eq!(waiting.terminate(None), (Ok(()), vec![Act::KillSessions]));
    }

    /// A cancel keeps the machine running and tells everyone; once killing
    /// has begun it is too late; with nothing pending, both are refused.
    #[test]
    fn a_cancel_keeps_the_machine_running_until_killing_begins() {
        let mut stops = Stops::default();
        assert_eq!(stops.cancel().0, Err(Refused::NoStopPending));
        assert_eq!(stops.terminate(None).0, Err(Refused::NoStopPending));
        let _ = stops.ask(WAIT, 1);
        assert_eq!(stops.cancel(), (Ok(()), vec![Act::CancelSessions, Act::Cancelled]));
        assert_eq!(stops.pending(), None);
        assert_eq!(stops.sessions(0), Vec::new(), "a cancelled stop stopped the machine");
        let _ = stops.ask(WAIT, 1);
        let _ = stops.terminate(None);
        assert_eq!(stops.cancel().0, Err(Refused::TooLate));
    }

    /// A terminate that names its stop forces that stop alone: once it is
    /// cancelled, a later stop of either kind is never forced by it.
    #[test]
    fn a_terminate_of_a_stop_forces_no_later_one() {
        let mut stops = Stops::default();
        let (Ok(first), _) = stops.ask(WAIT, 1) else { panic!("the first stop was refused") };
        let _ = stops.cancel();
        for later in [Request { stop: Stop::Reboot, mode: Mode::Wait }, WAIT] {
            let (Ok(ticket), _) = stops.ask(later, 1) else { panic!("{later:?} was refused") };
            assert_ne!(ticket, first, "a later stop was given a cancelled stop's ticket");
            assert_eq!(stops.terminate(Some(first)), (Err(Refused::NoStopPending), Vec::new()), "{later:?} was forced");
            assert_eq!(stops.terminate(Some(ticket)), (Ok(()), vec![Act::KillSessions]));
            stops.refused();
        }
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
        assert!(!owed.owes() && owed.done());
        owed.post(5, b"d".to_vec(), false, |_, _| panic!("a later stop's list was sent past the outcome"));
        owed.post(6, Vec::new(), true, |_, _| panic!("a second outcome was sent"));
        assert!(!owed.owes());
        let mut quick = Owed::default();
        quick.post(6, Vec::new(), true, |_, _| true);
        assert!(quick.done(), "an outcome sent at once left its watcher enrolled");
    }
}
