//! The machine's stop, from a request on `power` to the kernel's call
//! ([`toyos_supervisor::stop`] decides; this does).
//!
//! A stop is answered at once with its ticket and what it waits for. Each
//! requester that was answered is a watcher of that stop, told every list
//! after and the outcome, never blocking the loop: a watcher that does not
//! read is owed only the latest list and the outcome ([`Owed`]), and is let
//! go once the outcome is sent. A requester that hangs up cancels nothing.
//!
//! **The kernel's refusals are said in the protocol's words**: a stop the
//! kernel says before anything ends that this machine lacks is
//! [`Refused::Unsupported`], and a stop that was begun and refused, or any
//! other refusal, [`Refused::Failed`], its kernel error in the log.

use toyos::ipc::{Connection, FrameRx, RxStep};
use toyos::power::{self, Answer, Refused, Request, Stop, Waiting};
use toyos::{say, AsHandle};
use toyos_abi::syscall::{MachineStop, SyscallError};
use toyos_session as wire;
use toyos_supervisor::stop::{Act, Owed};

use crate::{Supervisor, STOPPING};

/// The watchers kept at once; a requester past them is answered and not
/// told more.
pub(crate) const MAX_WATCHERS: usize = 8;

pub(crate) struct Watcher {
    pub conn: Connection,
    /// A watcher says nothing more: any frame, or its hang-up, ends its watch.
    rx: FrameRx<8>,
    owed: Owed,
}

impl Watcher {
    /// Send what it is owed, if it may take it now.
    fn flush(&mut self) {
        let conn = &self.conn;
        self.owed.flush(|msg, payload| conn.try_send_bytes(msg, payload).is_ok());
    }

    pub fn owes(&self) -> bool {
        self.owed.owes()
    }

    /// Kept while its stop is pending and its outcome unsent, or one is owed.
    fn kept(&self, pending: bool) -> bool {
        self.owed.owes() || (pending && !self.owed.done())
    }

    fn post(&mut self, msg: u32, payload: Vec<u8>, outcome: bool) {
        let conn = &self.conn;
        self.owed.post(msg, payload, outcome, |msg, payload| conn.try_send_bytes(msg, payload).is_ok());
    }
}

impl Supervisor<'_> {
    /// One request on `power`.
    pub(crate) fn serve_power(&mut self, conn: Connection, msg_type: u32, payload: &[u8]) {
        let open = self.logins.open.len();
        let (answer, acts) = match msg_type {
            power::MSG_STOP => match Request::decode(payload) {
                Some(asked) => {
                    say!("supervisor: power: {:?} asked, {:?}", asked.stop, asked.mode);
                    match self.machine_stops(asked.stop) {
                        Ok(()) => self.stops.ask(asked, open),
                        Err(why) => (Err(why), Vec::new()),
                    }
                }
                None => return say!("supervisor: power: dropping a stop no requester writes"),
            },
            power::MSG_TERMINATE => {
                let of = match payload {
                    [] => None,
                    ticket => match <[u8; 8]>::try_from(ticket) {
                        Ok(ticket) => Some(u64::from_le_bytes(ticket)),
                        Err(_) => return say!("supervisor: power: dropping a terminate no requester writes"),
                    },
                };
                let (done, acts) = self.stops.terminate(of);
                (done.map(|()| 0), acts)
            }
            power::MSG_CANCEL => {
                let (done, acts) = self.stops.cancel();
                (done.map(|()| 0), acts)
            }
            other => return say!("supervisor: power: dropping client {} — frame {other} is no request", conn.as_handle().raw().0),
        };
        match answer {
            Ok(ticket) if msg_type == power::MSG_STOP => {
                let watched = self.watchers.len() < MAX_WATCHERS;
                let answer = Answer { ticket, watched, waiting: self.waiting() };
                let _ = conn.try_send_bytes(power::MSG_PENDING, &answer.encode());
                match watched {
                    true => self.watchers.push(Watcher { conn, rx: FrameRx::new(), owed: Owed::default() }),
                    false => say!("supervisor: power: {MAX_WATCHERS} watch the stop already; this requester is told no more"),
                }
            }
            Ok(_) => {
                let _ = conn.try_signal(power::MSG_DONE);
            }
            Err(why) => {
                say!("supervisor: power: refused: {why:?}");
                let _ = conn.try_send_bytes(power::MSG_REFUSED, &why.encode().expect("a refusal the supervisor says"));
            }
        }
        self.stop_acts(acts);
    }

    /// The watchers that woke: each sent what it is owed, and one that hung
    /// up or spoke let go.
    pub(crate) fn pump_watchers(&mut self) {
        let pending = self.stops.pending().is_some();
        self.watchers.retain_mut(|w| {
            w.flush();
            matches!(w.rx.pump(&w.conn), RxStep::Idle) && w.kept(pending)
        });
    }

    /// A new stop the kernel would refuse is refused before a session is
    /// asked to end for it.
    fn machine_stops(&self, how: Stop) -> Result<(), Refused> {
        if self.stops.pending().is_some() {
            return Ok(());
        }
        let stop = match how {
            Stop::Reboot => MachineStop::Reboot,
            Stop::Shutdown => MachineStop::Shutdown,
        };
        self.syscap.can_stop(stop).map_err(|kernel| match kernel {
            SyscallError::NotSupported => {
                say!("supervisor: power: this machine has no {how:?}");
                Refused::Unsupported
            }
            other => {
                toyos::error!("supervisor: power: the kernel refused to say whether it can {how:?}: {other:?}");
                Refused::Failed
            }
        })
    }

    /// What the stop waits for: every session's report.
    fn waiting(&self) -> Waiting {
        Waiting::of(self.logins.waiting_names())
    }

    /// The sessions changed: the pending stop reports, or stops once none is.
    pub(crate) fn sessions_changed(&mut self) {
        let acts = self.stops.sessions(self.logins.open.len());
        self.stop_acts(acts);
    }

    pub(crate) fn stop_acts(&mut self, acts: Vec<Act>) {
        for act in acts {
            match act {
                Act::AskSessions => self.logins.tell_all(wire::MSG_STOPPING),
                Act::CancelSessions => self.logins.tell_all(wire::MSG_CANCEL),
                Act::KillSessions => {
                    say!("supervisor: power: the stop ends what stays: {}", self.waiting().names.join(", "));
                    self.logins.kill_all();
                }
                Act::Report => {
                    let list = self.waiting().encode();
                    self.watchers.iter_mut().for_each(|w| w.post(power::MSG_WAITING, list.clone(), false));
                }
                Act::Cancelled => {
                    say!("supervisor: power: the stop is cancelled; the machine runs on");
                    self.watchers.iter_mut().for_each(|w| w.post(power::MSG_CANCELLED, Vec::new(), true));
                }
                Act::Stop(how) => self.halt(how),
            }
        }
        let pending = self.stops.pending().is_some();
        self.watchers.retain(|w| w.kept(pending));
    }

    /// `logkeeper` flushed, then the kernel asked with the machine's
    /// capability. Returns only where the kernel refused, which every watcher
    /// is told.
    fn halt(&mut self, how: Stop) {
        say!("{STOPPING} ({how:?})");
        self.log.flush();
        self.sync_files();
        let stopped = self.stop_drivers();
        let kernel = match how {
            Stop::Reboot => self.syscap.reboot(),
            Stop::Shutdown => self.syscap.shutdown(),
        };
        self.log.resume();
        toyos::error!("supervisor: power: the kernel refused {how:?}: {kernel:?}");
        // Their functions are owed another driver, as after any end.
        if stopped {
            self.drive();
        }
        self.stops.refused();
        let word = Refused::Failed.encode().expect("a refusal the supervisor says");
        self.watchers.iter_mut().for_each(|w| w.post(power::MSG_REFUSED, word.clone(), true));
    }
}

