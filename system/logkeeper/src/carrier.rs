//! The machine's console carrier, asked for on [`toyos_logstream::CARRIER`]:
//! the one reader the console UART goes quiet for.
//!
//! **The UART goes quiet once the carrier's channel has taken the log's first
//! byte, and comes back once the carrier goes, the machine stops, or the
//! channel takes nothing of what it is owed for [`STALL`]** ([`Carriage`]):
//! a carrier that is stuck but alive never leaves the UART silent. The UART
//! is quiet again once the channel takes more; meanwhile both carry the log.
//!
//! **A slow channel is never let go**: the carrier is handed the boot from
//! its first line like any reader, and keeps it however long its channel
//! takes. It says on its connection how much of its pipe its channel has
//! taken ([`DELIVERED`]), which is how the stop knows the channel has the log
//! up to the flush before the UART takes the rest ([`Carrier::stop`]).
//!
//! **This program's own end is the kernel's to answer**: the console it says
//! the log is carried on goes with this process, and the kernel gives the log
//! back to the UART with it, so a crash here cannot leave the UART quiet.
//!
//! What the carrier's channel had not taken when the carrier went is on
//! neither (`issues/the-console-loses-what-its-carrier-had-not-delivered.md`).

use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use toyos::ipc::{Connection, FrameRx, RxStep};
use toyos::poller::{Poller, READABLE, WRITABLE};
use toyos::port::Acceptor;
use toyos::{say, Console, Pipe};
use toyos_abi::syscall::SyscallError;
use toyos_logstream::{CARRIER_STALLED, CARRY, DELIVERED};

use crate::serve::Hub;

/// How long the stop waits for the carrier's channel to take the log up to
/// the flush: policy, well inside the supervisor's flush bound, and far past
/// what a channel that takes bytes at all needs for the last rounds.
const STOP_WAIT: Duration = Duration::from_secs(2);

/// How long the carrier's channel may take nothing of the bytes it is owed
/// before the UART carries the log too. Policy, short because the costs are
/// lopsided: a false stall only duplicates lines on both channels, and a
/// missed one leaves both silent.
pub const STALL: Duration = Duration::from_secs(5);


/// What the kernel is to be told about the console UART.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tell {
    /// The log is carried elsewhere: the UART takes only the stop's and a
    /// fatal path's lines.
    Quiet,
    /// The UART carries the log again.
    Loud,
}

/// Whether the UART is quiet, decided from what the carrier and the stop do.
/// Pure: each event answers what the kernel is told, if anything.
#[derive(Debug, Default)]
pub struct Carriage {
    /// The carrier admitted, by the number of its admission.
    carrier: Option<u64>,
    admitted: u64,
    /// Bytes written into its pipe.
    sent: u64,
    /// Bytes of its pipe the carrier's channel has taken.
    delivered: u64,
    /// Since when bytes have been owed and none taken.
    owed_since: Option<Instant>,
    /// The channel took nothing of what it was owed for [`STALL`].
    stalled: bool,
    stopping: bool,
    quiet: bool,
}

/// Why a carrier is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// One is already admitted.
    Busy,
    /// A count below one it gave already, or past the bytes it was sent.
    Count { said: u64, delivered: u64, sent: u64 },
}

impl Carriage {
    /// A carrier asks: its admission's number, or [`Refused::Busy`].
    pub fn admit(&mut self) -> Result<u64, Refused> {
        if self.carrier.is_some() {
            return Err(Refused::Busy);
        }
        self.admitted += 1;
        self.carrier = Some(self.admitted);
        self.sent = 0;
        self.delivered = 0;
        self.owed_since = None;
        self.stalled = false;
        Ok(self.admitted)
    }

    /// `bytes` more were written into carrier `who`'s pipe at `now`: whether
    /// `who` is still the carrier.
    pub fn fed(&mut self, who: u64, bytes: u64, now: Instant) -> bool {
        if self.carrier != Some(who) {
            return false;
        }
        if bytes > 0 && self.owed_since.is_none() {
            self.owed_since = Some(now);
        }
        self.sent += bytes;
        true
    }

    /// Carrier `who`'s channel has taken `said` bytes of its pipe, at `now`.
    pub fn delivered(&mut self, who: u64, said: u64, now: Instant) -> Result<Option<Tell>, Refused> {
        if self.carrier != Some(who) {
            return Ok(None);
        }
        if said < self.delivered || said > self.sent {
            return Err(Refused::Count { said, delivered: self.delivered, sent: self.sent });
        }
        if said > self.delivered {
            self.stalled = false;
            self.owed_since = (said < self.sent).then_some(now);
        }
        self.delivered = said;
        Ok(self.settle())
    }

    /// When the channel will have taken nothing of what it is owed for
    /// [`STALL`], while it is not already said to have.
    pub fn stall_at(&self) -> Option<Instant> {
        let watched = self.carrier.is_some() && !self.stalled && !self.stopping;
        self.owed_since.filter(|_| watched).map(|since| since + STALL)
    }

    /// The time is `now`: a channel past [`stall_at`](Self::stall_at) has
    /// stalled, and the UART carries the log too.
    pub fn tick(&mut self, now: Instant) -> Option<Tell> {
        if self.stall_at().is_some_and(|at| at <= now) {
            self.stalled = true;
        }
        self.settle()
    }

    /// Bytes owed the carrier's channel: sent and not yet taken.
    pub fn owed(&self) -> u64 {
        self.sent - self.delivered
    }

    /// Carrier `who` went: by its end, or by a count refused.
    pub fn gone(&mut self, who: u64) -> Option<Tell> {
        if self.carrier != Some(who) {
            return None;
        }
        self.carrier = None;
        self.settle()
    }

    /// The machine stops: the UART takes what the log says from here.
    pub fn stop(&mut self) -> Option<Tell> {
        self.stopping = true;
        self.settle()
    }

    /// The stop was refused.
    pub fn resume(&mut self) -> Option<Tell> {
        self.stopping = false;
        self.settle()
    }

    pub fn quiet(&self) -> bool {
        self.quiet
    }

    fn settle(&mut self) -> Option<Tell> {
        let quiet = self.carrier.is_some() && self.delivered > 0 && !self.stopping && !self.stalled;
        if quiet == self.quiet {
            return None;
        }
        self.quiet = quiet;
        Some(if quiet { Tell::Quiet } else { Tell::Loud })
    }
}

/// The carrier's state, shared by its threads and the main loop.
pub struct Carrier {
    console: Arc<Console>,
    state: Mutex<State>,
    /// Posted when the carrier's channel takes bytes, its feed moves, or it goes.
    moved: Condvar,
}

struct State {
    carriage: Carriage,
    /// The replay offset the bytes written into the carrier's pipe reach.
    at: u64,
}

impl Carrier {
    /// Serve the carrier on `acceptor` for the life of the process, telling
    /// the kernel through `console`.
    pub fn serve(acceptor: Acceptor, console: Arc<Console>, hub: Arc<Hub>) -> Arc<Self> {
        let carrier = Arc::new(Self {
            console,
            state: Mutex::new(State { carriage: Carriage::default(), at: 0 }),
            moved: Condvar::new(),
        });
        let theirs = Arc::clone(&carrier);
        std::thread::Builder::new()
            .name("log-carrier".into())
            .spawn(move || theirs.run(&acceptor, &hub))
            .expect("logkeeper: the carrier's thread could not be started");
        let theirs = Arc::clone(&carrier);
        std::thread::Builder::new()
            .name("log-carrier-stall".into())
            .spawn(move || theirs.watch())
            .expect("logkeeper: the carrier's stall thread could not be started");
        carrier
    }

    /// Whether the UART is quiet, so program lines are not written to it.
    pub fn quiet(&self) -> bool {
        self.lock().carriage.quiet()
    }

    /// The machine stops, and `end` is how long the boot is at the flush:
    /// wait, bounded, for the carrier's channel to take it all, then give the
    /// UART what the log says from here.
    pub fn stop(&self, end: u64) {
        let began = Instant::now();
        let mut state = self.lock();
        loop {
            let caught_up = state.at >= end && state.carriage.owed() == 0;
            if state.carriage.carrier.is_none() || caught_up {
                break;
            }
            let Some(left) = STOP_WAIT.checked_sub(began.elapsed()) else {
                say!(
                    "logkeeper: the console carrier's channel had taken {} of {} bytes {} ms into the stop; \
                     the UART takes the stop's lines, and /log has the rest",
                    state.carriage.delivered,
                    state.carriage.sent,
                    STOP_WAIT.as_millis()
                );
                break;
            };
            state = self.moved.wait_timeout(state, left).expect("logkeeper: the carrier's state is poisoned").0;
        }
        let tell = state.carriage.stop();
        self.tell(tell);
    }

    /// The stop was refused: the carrier carries the log again, past what
    /// the UART carried meanwhile ([`Hub::append_carried`]).
    pub fn resume(&self) {
        let mut state = self.lock();
        let tell = state.carriage.resume();
        self.tell(tell);
    }

    /// Watch the carrier's channel for [`STALL`], telling the kernel the UART
    /// carries the log while the channel takes nothing it is owed. Waits on
    /// the carrier's state changing where nothing is owed.
    fn watch(&self) -> ! {
        let mut state = self.lock();
        loop {
            let Some(at) = state.carriage.stall_at() else {
                state = self.moved.wait(state).expect("logkeeper: the carrier's state is poisoned");
                continue;
            };
            let now = Instant::now();
            if at > now {
                state = self.moved.wait_timeout(state, at - now).expect("logkeeper: the carrier's state is poisoned").0;
                continue;
            }
            let owed = state.carriage.owed();
            let tell = state.carriage.tick(now);
            // A channel that never took the first byte left the UART loud.
            if tell == Some(Tell::Loud) {
                say!(
                    "{CARRIER_STALLED} {owed} bytes owed it for {} s; the UART carries the log too until it takes more",
                    STALL.as_secs()
                );
            }
            self.tell(tell);
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("logkeeper: the carrier's state is poisoned")
    }

    /// Tell the kernel, under the state's lock, so two threads' changes reach
    /// it in the order they were decided.
    fn tell(&self, tell: Option<Tell>) {
        let Some(tell) = tell else { return };
        match self.console.carry_elsewhere(tell == Tell::Quiet) {
            Ok(()) => match tell {
                Tell::Quiet => say!("logkeeper: the console carrier's channel takes the log, so the UART is quiet"),
                Tell::Loud => say!("logkeeper: the UART carries the log again"),
            },
            // A fatal path or the stop gave the UART the log for good.
            Err(SyscallError::NotSupported) => {}
            Err(e) => panic!("logkeeper: the kernel refused to be told where the log is carried: {e:?}"),
        }
    }

    /// The acceptor and the admitted carrier's connection.
    fn run(self: &Arc<Self>, acceptor: &Acceptor, hub: &Arc<Hub>) -> ! {
        const ACCEPT: u64 = 0;
        const CONNECTION: u64 = 1;
        let poller = Poller::new(2);
        let mut admitted: Option<(u64, Connection, FrameRx<8>)> = None;
        // A connection that has not asked yet.
        let mut asking: Option<(Connection, FrameRx<0>)> = None;
        loop {
            poller.watch(acceptor, READABLE, ACCEPT);
            if let Some((_, conn, _)) = &admitted {
                poller.watch(conn, READABLE, CONNECTION);
            } else if let Some((conn, _)) = &asking {
                poller.watch(conn, READABLE, CONNECTION);
            }
            let mut ready = [false; 2];
            poller.wait(1, u64::MAX, |token| ready[token as usize] = true);

            if ready[ACCEPT as usize]
                && let Ok(conn) = acceptor.accept()
            {
                if admitted.is_some() || asking.is_some() {
                    say!("logkeeper: refusing a second console carrier: one is served");
                } else {
                    asking = Some((conn, FrameRx::new()));
                }
            }
            if !ready[CONNECTION as usize] {
                continue;
            }
            if let Some((conn, mut rx)) = asking.take() {
                match rx.pump(&conn) {
                    RxStep::Idle => asking = Some((conn, rx)),
                    RxStep::Frame { msg_type: CARRY, .. } => admitted = self.admit(conn, hub),
                    RxStep::Eof | RxStep::Malformed | RxStep::Frame { .. } => {}
                }
                continue;
            }
            let Some((who, conn, mut rx)) = admitted.take() else { continue };
            if self.read(who, &conn, &mut rx) {
                admitted = Some((who, conn, rx));
            }
        }
    }

    /// Hand the carrier on `conn` its pipe, and feed it from the boot's first
    /// line on a thread of its own.
    fn admit(self: &Arc<Self>, conn: Connection, hub: &Arc<Hub>) -> Option<(u64, Connection, FrameRx<8>)> {
        let who = match self.lock().carriage.admit() {
            Ok(who) => who,
            Err(why) => {
                say!("logkeeper: refusing a console carrier: {why:?}");
                return None;
            }
        };
        let Some(pipe) = hub.hand_over(&conn) else {
            self.went(who);
            return None;
        };
        self.lock().at = 0;
        let (carrier, hub) = (Arc::clone(self), Arc::clone(hub));
        let fed = std::thread::Builder::new().name("log-carried".into()).spawn(move || {
            carrier.feed(who, &pipe, &hub);
            carrier.went(who);
        });
        if let Err(e) = fed {
            say!("logkeeper: no thread for the console carrier: {e}");
            self.went(who);
            return None;
        }
        Some((who, conn, FrameRx::new()))
    }

    /// Every frame carrier `who` sent: whether it is still served.
    fn read(&self, who: u64, conn: &Connection, rx: &mut FrameRx<8>) -> bool {
        loop {
            match rx.pump(conn) {
                RxStep::Idle => return true,
                RxStep::Frame { msg_type: DELIVERED, payload_len: 8, .. } => {
                    let said = u64::from_le_bytes(rx.payload(8).try_into().expect("eight bytes"));
                    let mut state = self.lock();
                    match state.carriage.delivered(who, said, Instant::now()) {
                        Ok(tell) => {
                            self.tell(tell);
                            drop(state);
                            self.moved.notify_all();
                        }
                        Err(why) => {
                            drop(state);
                            say!("logkeeper: letting the console carrier go: {why:?}");
                            self.went(who);
                            return false;
                        }
                    }
                }
                RxStep::Eof | RxStep::Malformed | RxStep::Frame { .. } => {
                    self.went(who);
                    return false;
                }
            }
        }
    }

    fn went(&self, who: u64) {
        let mut state = self.lock();
        let tell = state.carriage.gone(who);
        self.tell(tell);
        drop(state);
        self.moved.notify_all();
    }

    /// Write the boot into carrier `who`'s pipe until the pipe's reader goes.
    fn feed(&self, who: u64, pipe: &Pipe, hub: &Hub) {
        let poller = Poller::new(1);
        let mut at = 0u64;
        loop {
            let chunk = hub.next_for_the_carrier(&mut at);
            // Counted before the pipe has it, so no count the carrier gives
            // of these bytes is ever past what it was sent.
            if !self.lock().carriage.fed(who, chunk.len() as u64, Instant::now()) {
                return;
            }
            self.moved.notify_all();
            let mut written = 0;
            while written < chunk.len() {
                match pipe.write_nonblock(&chunk[written..]) {
                    Ok(n) => written += n,
                    Err(SyscallError::WouldBlock) => {
                        // No bound: a slow channel is not a failed one, and
                        // the pipe's reader going ends the wait.
                        poller.watch(pipe, WRITABLE, 0);
                        poller.wait(1, u64::MAX, |_| {});
                    }
                    Err(_) => return,
                }
            }
            self.lock().at = at;
            self.moved.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A carrier admitted and fed `sent` bytes at `at`.
    fn fed(carriage: &mut Carriage, sent: u64, at: Instant) -> u64 {
        let who = carriage.admit().expect("the carrier is admitted");
        assert!(carriage.fed(who, sent, at));
        who
    }

    /// The UART goes quiet at the carrier's first delivered byte, not at its
    /// admission: a carrier whose channel takes nothing has carried nothing.
    #[test]
    fn the_uart_goes_quiet_once_the_carriers_channel_takes_a_byte() {
        let t = Instant::now();
        let mut carriage = Carriage::default();
        let who = fed(&mut carriage, 100, t);
        assert!(!carriage.quiet());
        assert_eq!(carriage.delivered(who, 0, t), Ok(None));
        assert_eq!(carriage.delivered(who, 40, t), Ok(Some(Tell::Quiet)));
        assert_eq!(carriage.delivered(who, 100, t), Ok(None));
        assert!(carriage.quiet());
    }

    /// The carrier's end, its driver's death among them, gives the log back
    /// to the UART.
    #[test]
    fn the_carriers_end_gives_the_log_back_to_the_uart() {
        let t = Instant::now();
        let mut carriage = Carriage::default();
        let who = fed(&mut carriage, 1, t);
        carriage.delivered(who, 1, t).expect("a count it was sent");
        assert_eq!(carriage.gone(who), Some(Tell::Loud));
        assert!(!carriage.quiet());
        assert_eq!(carriage.gone(who), None, "one end is answered once");
        // A later carrier carries it again from its own first byte.
        let next = fed(&mut carriage, 5, t);
        assert_ne!(next, who);
        assert_eq!(carriage.delivered(who, 5, t), Ok(None), "the gone carrier's counts are not this one's");
        assert!(!carriage.fed(who, 1, t), "nor are its feeds");
        assert_eq!(carriage.delivered(next, 5, t), Ok(Some(Tell::Quiet)));
    }

    /// A carrier that went before it delivered anything leaves the UART as it was.
    #[test]
    fn a_carrier_that_carried_nothing_changes_nothing() {
        let mut carriage = Carriage::default();
        let who = carriage.admit().expect("the first carrier is admitted");
        assert_eq!(carriage.gone(who), None);
    }

    #[test]
    fn a_second_carrier_is_refused_while_one_is_served() {
        let mut carriage = Carriage::default();
        carriage.admit().expect("the first carrier is admitted");
        assert_eq!(carriage.admit(), Err(Refused::Busy));
    }

    /// A count the carrier could not have, or one going backwards, is refused,
    /// and the main loop lets the carrier go on it.
    #[test]
    fn a_count_past_what_was_sent_or_backwards_is_refused() {
        let t = Instant::now();
        let mut carriage = Carriage::default();
        let who = fed(&mut carriage, 10, t);
        assert_eq!(carriage.delivered(who, 11, t), Err(Refused::Count { said: 11, delivered: 0, sent: 10 }));
        carriage.delivered(who, 8, t).expect("a count it was sent");
        assert_eq!(carriage.delivered(who, 7, t), Err(Refused::Count { said: 7, delivered: 8, sent: 10 }));
        assert_eq!(carriage.gone(who), Some(Tell::Loud));
    }

    /// The stop gives the UART the log, and a refused stop gives it back to
    /// a carrier that is still there.
    #[test]
    fn the_stop_takes_the_log_to_the_uart_and_a_refused_one_gives_it_back() {
        let t = Instant::now();
        let mut carriage = Carriage::default();
        let who = fed(&mut carriage, 3, t);
        carriage.delivered(who, 3, t).expect("a count it was sent");
        assert_eq!(carriage.stop(), Some(Tell::Loud));
        carriage.fed(who, 1, t);
        assert_eq!(carriage.delivered(who, 4, t), Ok(None), "nothing quiets the UART while the machine stops");
        assert_eq!(carriage.resume(), Some(Tell::Quiet));

        assert_eq!(carriage.stop(), Some(Tell::Loud));
        assert_eq!(carriage.gone(who), None);
        assert_eq!(carriage.resume(), None, "no carrier is left to carry it");
        assert!(!carriage.quiet());
    }

    /// **A channel that takes nothing it is owed for [`STALL`] has the UART
    /// carry the log too**, and taking more quiets it again: a stuck carrier
    /// never leaves the log on neither channel. The bound runs from the last
    /// progress, not from the first byte owed.
    #[test]
    fn a_channel_that_stops_taking_the_log_has_the_uart_carry_it_until_it_takes_more() {
        let t = Instant::now();
        let mut carriage = Carriage::default();
        let who = fed(&mut carriage, 10, t);
        carriage.delivered(who, 4, t).expect("a count it was sent");
        assert!(carriage.quiet());
        assert_eq!(carriage.stall_at(), Some(t + STALL));
        assert_eq!(carriage.tick(t + STALL / 2), None);
        // Progress moves the bound.
        let later = t + STALL / 2;
        carriage.delivered(who, 6, later).expect("a count it was sent");
        assert_eq!(carriage.tick(t + STALL), None, "the bound runs from the last progress");
        assert_eq!(carriage.tick(later + STALL), Some(Tell::Loud));
        assert!(!carriage.quiet());
        assert_eq!(carriage.stall_at(), None, "a stall is said once");
        assert_eq!(carriage.tick(later + STALL * 3), None);
        // More fed while stalled keeps the UART carrying it.
        carriage.fed(who, 5, later + STALL * 3);
        assert_eq!(carriage.tick(later + STALL * 9), None);
        let resumed = later + STALL * 10;
        assert_eq!(carriage.delivered(who, 7, resumed), Ok(Some(Tell::Quiet)));
        assert_eq!(carriage.stall_at(), Some(resumed + STALL));
        assert_eq!(carriage.delivered(who, 15, resumed), Ok(None));
        assert_eq!(carriage.stall_at(), None, "nothing is owed");
        assert_eq!(carriage.tick(resumed + STALL * 100), None);
    }

    /// Nothing owed is nothing stalled, however long the log is idle, and the
    /// bound starts at the first byte owed after it.
    #[test]
    fn an_idle_log_is_no_stall() {
        let t = Instant::now();
        let mut carriage = Carriage::default();
        let who = fed(&mut carriage, 3, t);
        carriage.delivered(who, 3, t).expect("a count it was sent");
        assert_eq!(carriage.tick(t + STALL * 10), None);
        let fed_at = t + STALL * 10;
        carriage.fed(who, 2, fed_at);
        assert_eq!(carriage.stall_at(), Some(fed_at + STALL));
    }
}
