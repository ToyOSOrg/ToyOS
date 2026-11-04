//! The console's carriers: a driver that asks for the log here, writes it to
//! its channel and says how much of its pipe the channel has taken
//! ([`DELIVERED`]). Two channels, one serving ([`Carriers`]): the VM's console
//! port, asked for on [`toyos_logstream::CARRIER`], and the console UART, on
//! [`toyos_logstream::WIRE`]. A carrier is admitted alone on its channel, fed
//! from where its channel starts, and let go with its connection.
//!
//! **The UART goes quiet once the port's channel has taken the log's first
//! byte, and comes back once the port's carrier goes, the machine stops, or
//! the port's channel takes nothing of what it is owed for [`STALL`]**
//! ([`Carriage`]): a carrier that is stuck but alive never leaves the UART
//! silent. The UART is quiet again once the channel takes more; meanwhile
//! both carry the log. The UART says each change on itself
//! ([`CARRIED_ELSEWHERE`], [`BACK_ON_THE_UART`]) and carries nothing between.
//!
//! **The port's carrier is handed the boot from its first line, and the
//! UART's from where the UART's last carrier left off, or else from the
//! hand-over with every line before it but the records the kernel put on the
//! UART itself** ([`Hub::wire_start`]). A slow channel is never let go. What a
//! channel has taken is told the kernel as the read positions of the rounds it
//! has taken whole (`toyos::log::spoken`), so a fatal path and the stop put on
//! the UART exactly the kernel's records no channel has delivered; a channel
//! handed a gap in the replay speaks for nothing after it.
//!
//! **This program's end is its carriers' pipes'**: what follows on the UART
//! is the kernel's stop or a fatal path, from what was last spoken.
//!
//! What a channel had not taken when its carrier went is on neither
//! (`issues/the-console-loses-what-its-carrier-had-not-delivered.md`).

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use toyos::ipc::{Connection, FrameRx, RxStep};
use toyos::poller::{Poller, READABLE, WRITABLE};
use toyos::port::Acceptor;
use toyos::syscap::SysCap;
use toyos::{say, Pipe};
use toyos_abi::log::Severity;
use toyos_abi::syscall::SyscallError;
use toyos_logstream::{BACK_ON_THE_UART, CARRIED_ELSEWHERE, CARRIER_STALLED, CARRY, DELIVERED};

use crate::serve::{Fed, Hub, Positions};

/// How long the stop waits for the channels to take the log up to the flush:
/// policy, well inside the supervisor's flush bound, and far past what a
/// channel that takes bytes at all needs for the last rounds.
const STOP_WAIT: Duration = Duration::from_secs(2);

/// How long the port's channel may take nothing of the bytes it is owed
/// before the UART carries the log too. Policy, short because the costs are
/// lopsided: a false stall only duplicates lines on both channels, and a
/// missed one leaves both silent.
pub const STALL: Duration = Duration::from_secs(5);

/// Which of the console's channels a carrier is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    /// The VM's console port, which the UART goes quiet for.
    Port,
    /// The console UART.
    Wire,
}

impl Channel {
    const ALL: [Self; 2] = [Self::Port, Self::Wire];

    fn name(self) -> &'static str {
        match self {
            Self::Port => "the console carrier's channel",
            Self::Wire => "the UART",
        }
    }
}

/// What a change of the port's carrying makes the UART.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tell {
    /// The log is carried elsewhere: the UART takes only the stop's and a
    /// fatal path's lines.
    Quiet,
    /// The UART carries the log again.
    Loud,
}

/// One channel's carrier, its counts and whether it carries the log: what
/// the UART's quiet is decided from on the port's. Pure: each event answers
/// the change it makes, if any.
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
    /// Its channel carries the log: it has taken bytes, has not stalled, and
    /// the machine is not stopping.
    carrying: bool,
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

    pub fn carrying(&self) -> bool {
        self.carrying
    }

    fn settle(&mut self) -> Option<Tell> {
        let carrying = self.carrier.is_some() && self.delivered > 0 && !self.stopping && !self.stalled;
        if carrying == self.carrying {
            return None;
        }
        self.carrying = carrying;
        Some(if carrying { Tell::Quiet } else { Tell::Loud })
    }
}

/// Both channels' carriers, shared by their threads and the main loop.
pub struct Carriers {
    hub: Arc<Hub>,
    /// What the kernel is told the console has spoken through.
    cap: Arc<SysCap>,
    state: Mutex<State>,
    /// Posted when a channel takes bytes, a feed moves, or a carrier goes.
    moved: Condvar,
}

struct State {
    feeds: [Feed; 2],
    /// The positions the kernel was last told the console spoke through.
    spoken: Option<Positions>,
}

/// One channel's carrier and its feed.
#[derive(Default)]
struct Feed {
    carriage: Carriage,
    /// The replay offset the bytes written into the carrier's pipe reach.
    at: u64,
    /// Each piece of the replay written into the pipe: the pipe's length once
    /// it is written, and the replay offset it reaches.
    written: VecDeque<(u64, u64)>,
    /// The replay offset the channel has taken, or passed over while quiet,
    /// across its carriers: where the next one starts.
    resumed: u64,
    /// The carrier was handed a gap in the replay: it speaks for nothing.
    gap: bool,
}

impl State {
    fn feed(&mut self, channel: Channel) -> &mut Feed {
        &mut self.feeds[channel as usize]
    }

    /// Whether the UART is quiet: the port carries the log.
    fn quiet(&self) -> bool {
        self.feeds[Channel::Port as usize].carriage.carrying()
    }
}

impl Carriers {
    /// Serve each channel's carrier on its acceptor for the life of the
    /// process, telling the kernel what was spoken through `cap`.
    pub fn serve(port: Option<Acceptor>, wire: Option<Acceptor>, hub: Arc<Hub>, cap: Arc<SysCap>) -> Arc<Self> {
        let carriers = Arc::new(Self {
            hub,
            cap,
            state: Mutex::new(State { feeds: Default::default(), spoken: None }),
            moved: Condvar::new(),
        });
        for (channel, acceptor) in [(Channel::Port, port), (Channel::Wire, wire)] {
            let Some(acceptor) = acceptor else { continue };
            let theirs = Arc::clone(&carriers);
            std::thread::Builder::new()
                .name("log-carrier".into())
                .spawn(move || theirs.run(channel, &acceptor))
                .expect("logkeeper: a carrier's thread could not be started");
        }
        let theirs = Arc::clone(&carriers);
        std::thread::Builder::new()
            .name("log-carrier-stall".into())
            .spawn(move || theirs.watch())
            .expect("logkeeper: the carriers' stall thread could not be started");
        carriers
    }

    /// The machine stops, and `end` is how long the boot is at the flush:
    /// wait, bounded, for every channel that carries the log to take it all,
    /// then give the UART what the log says from here.
    pub fn stop(&self, end: u64) {
        let began = Instant::now();
        let mut state = self.lock();
        loop {
            let quiet = state.quiet();
            let behind = Channel::ALL.into_iter().find(|&channel| {
                let feed = &state.feeds[channel as usize];
                let carries = feed.carriage.carrier.is_some() && !(channel == Channel::Wire && quiet);
                carries && (feed.at < end || feed.carriage.owed() > 0)
            });
            let Some(channel) = behind else { break };
            let Some(left) = STOP_WAIT.checked_sub(began.elapsed()) else {
                let carriage = &state.feeds[channel as usize].carriage;
                say!(
                    "logkeeper: {} had taken {} of {} bytes {} ms into the stop; the UART takes the \
                     stop's lines, and /log has the rest",
                    channel.name(),
                    carriage.delivered,
                    carriage.sent,
                    STOP_WAIT.as_millis()
                );
                break;
            };
            state = self.moved.wait_timeout(state, left).expect("logkeeper: the carriers' state is poisoned").0;
        }
        let tell = state.feed(Channel::Port).carriage.stop();
        self.tell(Channel::Port, tell);
        drop(state);
        self.moved.notify_all();
    }

    /// The stop was refused: the port carries the log again.
    pub fn resume(&self) {
        let tell = self.lock().feed(Channel::Port).carriage.resume();
        self.tell(Channel::Port, tell);
        self.moved.notify_all();
    }

    /// Watch the port's channel for [`STALL`], letting the UART carry the log
    /// while the channel takes nothing it is owed. Waits on the carriers'
    /// state changing where nothing is owed.
    fn watch(&self) -> ! {
        let mut state = self.lock();
        loop {
            let Some(at) = state.feed(Channel::Port).carriage.stall_at() else {
                state = self.moved.wait(state).expect("logkeeper: the carriers' state is poisoned");
                continue;
            };
            let now = Instant::now();
            if at > now {
                state = self.moved.wait_timeout(state, at - now).expect("logkeeper: the carriers' state is poisoned").0;
                continue;
            }
            let carriage = &mut state.feed(Channel::Port).carriage;
            let owed = carriage.owed();
            let tell = carriage.tick(now);
            // A channel that never took the first byte left the UART loud.
            if tell == Some(Tell::Loud) {
                say!(
                    "{CARRIER_STALLED} {owed} bytes owed it for {} s; the UART carries the log too until it takes more",
                    STALL.as_secs()
                );
            }
            self.tell(Channel::Port, tell);
            self.moved.notify_all();
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("logkeeper: the carriers' state is poisoned")
    }

    /// Say a change of the UART's quiet in the log, which only the port's
    /// carrying makes; the UART's own feed says it on the UART.
    fn tell(&self, channel: Channel, tell: Option<Tell>) {
        if channel != Channel::Port {
            return;
        }
        match tell {
            Some(Tell::Quiet) => say!("logkeeper: the console carrier's channel takes the log, so the UART is quiet"),
            Some(Tell::Loud) => say!("logkeeper: the UART carries the log again"),
            None => {}
        }
    }

    /// The acceptor and the admitted carrier's connection, on `channel`.
    fn run(self: &Arc<Self>, channel: Channel, acceptor: &Acceptor) -> ! {
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
                    say!("logkeeper: refusing a second carrier for {}: one is served", channel.name());
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
                    RxStep::Frame { msg_type: CARRY, .. } => admitted = self.admit(channel, conn),
                    RxStep::Eof | RxStep::Malformed | RxStep::Frame { .. } => {}
                }
                continue;
            }
            let Some((who, conn, mut rx)) = admitted.take() else { continue };
            if self.read(channel, who, &conn, &mut rx) {
                admitted = Some((who, conn, rx));
            }
        }
    }

    /// Hand the carrier on `conn` its pipe, and feed it on a thread of its own.
    fn admit(self: &Arc<Self>, channel: Channel, conn: Connection) -> Option<(u64, Connection, FrameRx<8>)> {
        let who = match self.lock().feed(channel).carriage.admit() {
            Ok(who) => who,
            Err(why) => {
                say!("logkeeper: refusing a carrier for {}: {why:?}", channel.name());
                return None;
            }
        };
        let Some(pipe) = self.hub.hand_over(&conn) else {
            self.went(channel, who);
            return None;
        };
        {
            let mut state = self.lock();
            let feed = state.feed(channel);
            feed.at = 0;
            feed.written.clear();
            feed.gap = false;
        }
        let carriers = Arc::clone(self);
        let fed = std::thread::Builder::new().name("log-carried".into()).spawn(move || {
            carriers.feed(channel, who, &pipe);
            carriers.went(channel, who);
        });
        if let Err(e) = fed {
            say!("logkeeper: no thread for a carrier for {}: {e}", channel.name());
            self.went(channel, who);
            return None;
        }
        Some((who, conn, FrameRx::new()))
    }

    /// Every frame carrier `who` sent: whether it is still served.
    fn read(&self, channel: Channel, who: u64, conn: &Connection, rx: &mut FrameRx<8>) -> bool {
        loop {
            match rx.pump(conn) {
                RxStep::Idle => return true,
                RxStep::Frame { msg_type: DELIVERED, payload_len: 8, .. } => {
                    let said = u64::from_le_bytes(rx.payload(8).try_into().expect("eight bytes"));
                    let mut state = self.lock();
                    match state.feed(channel).carriage.delivered(who, said, Instant::now()) {
                        Ok(tell) => {
                            self.tell(channel, tell);
                            self.spoken(&mut state, channel, said);
                            drop(state);
                            self.moved.notify_all();
                        }
                        Err(why) => {
                            drop(state);
                            say!("logkeeper: letting the carrier for {} go: {why:?}", channel.name());
                            self.went(channel, who);
                            return false;
                        }
                    }
                }
                RxStep::Eof | RxStep::Malformed | RxStep::Frame { .. } => {
                    self.went(channel, who);
                    return false;
                }
            }
        }
    }

    /// `channel` has taken `delivered` bytes of its pipe: tell the kernel the
    /// positions of the rounds that is all of, where they are not what it was
    /// told last.
    fn spoken(&self, state: &mut State, channel: Channel, delivered: u64) {
        let feed = state.feed(channel);
        let mut through = None;
        while feed.written.front().is_some_and(|&(end, _)| end <= delivered) {
            through = feed.written.pop_front().map(|(_, at)| at);
        }
        let Some(at) = through else { return };
        feed.resumed = feed.resumed.max(at);
        if feed.gap {
            return;
        }
        let Some(positions) = self.hub.positions_through(at) else { return };
        if state.spoken.as_ref().is_some_and(|told| Arc::ptr_eq(told, &positions)) {
            return;
        }
        if let Err(e) = toyos::log::spoken(&self.cap, &positions) {
            panic!("logkeeper: the kernel refused to be told what the console spoke: {e:?}");
        }
        state.spoken = Some(positions);
    }

    fn went(&self, channel: Channel, who: u64) {
        let tell = self.lock().feed(channel).carriage.gone(who);
        self.tell(channel, tell);
        self.moved.notify_all();
    }

    /// Write the log into carrier `who`'s pipe until the pipe's reader goes:
    /// the port's from the boot's first line, the UART's from where its
    /// channel left off ([`Hub::wire_start`]), and nothing of what the port
    /// carries.
    fn feed(&self, channel: Channel, who: u64, pipe: &Pipe) {
        let poller = Poller::new(1);
        let wire = channel == Channel::Wire;
        let (mut at, mut owed) = if wire {
            let resumed = self.lock().feed(channel).resumed;
            self.hub.wire_start(resumed)
        } else {
            (0, None)
        };
        // Whether the UART last said the port carries the log.
        let mut said_quiet = false;
        loop {
            let fed = match owed.take() {
                Some(owed) => Fed::Replay(owed.to_vec()),
                None => self.hub.next_for_a_carrier(&mut at, wire),
            };
            if wire {
                let quiet = self.lock().quiet();
                if quiet != said_quiet {
                    said_quiet = quiet;
                    let line = if quiet { CARRIED_ELSEWHERE } else { BACK_ON_THE_UART };
                    if !self.write(channel, who, pipe, &poller, &self.hub.own_line(Severity::Info, line), None) {
                        return;
                    }
                }
                if quiet {
                    // Passed over, which is what the stop's wait reads, and
                    // where a carrier started again resumes.
                    let mut state = self.lock();
                    let feed = state.feed(channel);
                    feed.at = at;
                    feed.resumed = feed.resumed.max(at);
                    continue;
                }
            }
            let (bytes, reaches) = match fed {
                Fed::Replay(bytes) => (bytes, Some(at)),
                Fed::Said(bytes) => (bytes, None),
                Fed::Gap(bytes) => {
                    self.lock().feed(channel).gap = true;
                    (bytes, None)
                }
            };
            if !self.write(channel, who, pipe, &poller, &bytes, reaches) {
                return;
            }
        }
    }

    /// `bytes` into carrier `who`'s pipe, which reach replay offset `reaches`
    /// where they are the replay's: whether `who` is still its channel's
    /// carrier and the pipe's reader is there.
    fn write(&self, channel: Channel, who: u64, pipe: &Pipe, poller: &Poller, bytes: &[u8], reaches: Option<u64>) -> bool {
        // Counted before the pipe has them, so no count the carrier gives of
        // these bytes is ever past what it was sent.
        {
            let mut state = self.lock();
            let feed = state.feed(channel);
            if !feed.carriage.fed(who, bytes.len() as u64, Instant::now()) {
                return false;
            }
            if let Some(at) = reaches {
                let sent = feed.carriage.sent;
                feed.written.push_back((sent, at));
            }
        }
        self.moved.notify_all();
        let mut written = 0;
        while written < bytes.len() {
            match pipe.write_nonblock(&bytes[written..]) {
                Ok(n) => written += n,
                Err(SyscallError::WouldBlock) => {
                    // No bound: a slow channel is not a failed one, and
                    // the pipe's reader going ends the wait.
                    poller.watch(pipe, WRITABLE, 0);
                    poller.wait(1, u64::MAX, |_| {});
                }
                Err(_) => return false,
            }
        }
        if let Some(at) = reaches {
            self.lock().feed(channel).at = at;
        }
        self.moved.notify_all();
        true
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
        assert!(!carriage.carrying());
        assert_eq!(carriage.delivered(who, 0, t), Ok(None));
        assert_eq!(carriage.delivered(who, 40, t), Ok(Some(Tell::Quiet)));
        assert_eq!(carriage.delivered(who, 100, t), Ok(None));
        assert!(carriage.carrying());
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
        assert!(!carriage.carrying());
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
        assert!(!carriage.carrying());
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
        assert!(carriage.carrying());
        assert_eq!(carriage.stall_at(), Some(t + STALL));
        assert_eq!(carriage.tick(t + STALL / 2), None);
        // Progress moves the bound.
        let later = t + STALL / 2;
        carriage.delivered(who, 6, later).expect("a count it was sent");
        assert_eq!(carriage.tick(t + STALL), None, "the bound runs from the last progress");
        assert_eq!(carriage.tick(later + STALL), Some(Tell::Loud));
        assert!(!carriage.carrying());
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
