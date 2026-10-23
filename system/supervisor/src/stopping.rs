//! The machine's stop, from a request on `power` to the kernel's call
//! ([`toyos_supervisor::stop`] decides; this does).
//!
//! A stop signs every login session out with a terminate and is answered at
//! once with what it waits for. Each requester that was answered is a
//! watcher, told every list after and the outcome, never blocking the loop:
//! a watcher that does not read is owed only the latest list and the outcome
//! ([`Owed`]). A requester that hangs up cancels nothing.

use toyos::ipc::{Connection, FrameRx, RxStep};
use toyos::power::{self, Refused, Request, Stop, Waiting};
use toyos::{say, AsHandle};
use toyos_abi::syscall::MachineStop;
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
            power::MSG_TERMINATE => self.stops.terminate(),
            power::MSG_CANCEL => self.stops.cancel(),
            other => return say!("supervisor: power: dropping client {} — frame {other} is no request", conn.as_handle().0),
        };
        match answer {
            Ok(()) if msg_type == power::MSG_STOP => {
                let _ = conn.try_send_bytes(power::MSG_PENDING, &self.waiting().encode());
                match self.watchers.len() < MAX_WATCHERS {
                    true => self.watchers.push(Watcher { conn, rx: FrameRx::new(), owed: Owed::default() }),
                    false => say!("supervisor: power: {MAX_WATCHERS} watch the stop already; this requester is told no more"),
                }
            }
            Ok(()) => {
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
            matches!(w.rx.pump(&w.conn), RxStep::Idle) && (pending || w.owes())
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
        self.syscap.stops(stop).map_err(Refused::Kernel)
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
        self.watchers.retain(|w| pending || w.owes());
    }

    /// `logkeeper` flushed, then the kernel asked with the machine's
    /// capability. Returns only where the kernel refused, which every watcher
    /// is told.
    fn halt(&mut self, how: Stop) {
        say!("{STOPPING} ({how:?})");
        self.log.flush();
        self.sync_files();
        let stopped = self.stop_drivers();
        let refused = match how {
            Stop::Reboot => self.syscap.reboot(),
            Stop::Shutdown => self.syscap.shutdown(),
        };
        self.log.resume();
        toyos::error!("supervisor: power: the kernel refused {how:?}: {refused:?}");
        // Their functions are owed another driver, as after any end.
        if stopped {
            self.drive();
        }
        self.stops.refused();
        let word = Refused::Kernel(refused).encode().expect("the kernel's refusal is said");
        self.watchers.iter_mut().for_each(|w| w.post(power::MSG_REFUSED, word.clone(), true));
    }
}
