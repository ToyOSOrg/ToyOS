//! The output drivers on the `sound-output` port (`toyos-sound-output`): their
//! offers, the one soundserver plays to, and its master.
//!
//! **The first offer soundserver can mix into is the one it plays to**, and
//! every later one is refused: a driver whose controller reaches no output
//! offers nothing, so on a machine with display audio beside an analog codec
//! the analog codec's driver is the one that offers. **Playing to it begins
//! when no stream is open**, so a stream never changes device under it: until
//! then soundserver is the null sink, and the offer waits.
//!
//! The control thread holds everything here but the stream connection, which
//! goes to the mix thread with the ring as [`Attached`]. **Nothing a driver
//! sends ends soundserver**: one that hangs up or breaks the protocol is
//! dropped by name, and soundserver is the null sink again ([`Event::Gone`]).

use toyos::ipc::{FrameRx, RxStep};
use toyos::poller::{Poller, READABLE};
use toyos::port::Acceptor;
use toyos::shm::{Bytes, SharedMemory};
use toyos::{AsHandle, Connection};
use toyos_sound_output::{
    run, FromDriver, Master, Offer, Played, MSG_ACCEPTED, MSG_MASTER_MUTE, MSG_MASTER_STEP, MSG_PLAYED, MSG_REFUSED,
    MSG_START, MSG_STOP,
};

use crate::backend::{Backend, Completion, Pipeline};

/// The widest frame a driver sends: an offer.
const MAX_FRAME: usize = Offer::BYTES;

/// Connections soundserver holds before their first frame names them. Policy:
/// a machine has a driver per audio function, two on the widest bench.
const MAX_UNNAMED: usize = 8;

/// An output, handed to the mix thread to play to.
pub(crate) struct Attached {
    pub(crate) backend: DriverBackend,
    pub(crate) offer: Offer,
    pub(crate) period_frames: usize,
    /// Which output this is, counted by the control thread: a stream opened
    /// against another is not mixed on it.
    pub(crate) generation: u32,
}

/// What a master request comes to on the control thread.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Asked {
    /// Sent to the driver; its answer comes back through [`Outputs::serve`].
    Forwarded,
    /// No output with a master: answered at once.
    Unavailable,
    /// The driver's connection took no request: answered unavailable, by name.
    Refused(String),
}

/// One event on the outputs, for the control thread to act on.
pub(crate) enum Event {
    /// The driver answered a master request.
    Master(Option<Master>),
    /// The driver played to is dropped, for this reason: soundserver is the
    /// null sink again.
    Gone(String),
}

/// What sending a request to the driver came to.
pub(crate) fn asked<E: core::fmt::Debug>(sent: Result<(), E>) -> Asked {
    match sent {
        Ok(()) => Asked::Forwarded,
        Err(why) => Asked::Refused(format!("the output driver took no master request ({why:?})")),
    }
}

enum Phase {
    /// No output, and an offer is taken.
    Open,
    /// An offer accepted and its ring lent: waiting for its stream connection.
    Accepted { control: Driver, offer: Offer, period_frames: usize, ring: SharedMemory },
    /// Its stream connection open: waiting for no stream to be open.
    Ready { control: Driver, attached: Box<Attached> },
    /// Played to, and whether its path has a master. `None` where
    /// soundserver plays to a device of its own and takes no driver.
    Playing(Option<Driver>, bool),
}

struct Driver {
    conn: Connection,
    rx: FrameRx<MAX_FRAME>,
}

/// The port and every connection on it.
pub(crate) struct Outputs {
    acceptor: Acceptor,
    unnamed: Vec<Driver>,
    phase: Phase,
}

impl Outputs {
    /// `own` is where soundserver plays to a device it drives itself, and so
    /// takes no driver.
    pub(crate) fn new(acceptor: Acceptor, own: bool) -> Self {
        let phase = if own { Phase::Playing(None, false) } else { Phase::Open };
        Self { acceptor, unnamed: Vec::new(), phase }
    }

    /// Watch every handle here, with tokens from `base` up.
    pub(crate) fn watch(&self, poller: &Poller, base: u64) {
        poller.watch(&self.acceptor, READABLE, base);
        let control = match &self.phase {
            Phase::Accepted { control, .. } | Phase::Ready { control, .. } | Phase::Playing(Some(control), _) => {
                Some(control)
            }
            Phase::Open | Phase::Playing(None, _) => None,
        };
        if let Some(control) = control {
            poller.watch(&control.conn, READABLE, base + 1);
        }
        for (i, driver) in self.unnamed.iter().enumerate() {
            poller.watch(&driver.conn, READABLE, base + 2 + i as u64);
        }
    }

    /// The tokens [`Self::watch`] can answer with.
    pub(crate) fn tokens(base: u64) -> core::ops::Range<u64> {
        base..base + 2 + MAX_UNNAMED as u64
    }

    /// Serve every connection `ready` names; the master answers the control
    /// thread is to pass on come back in `events`.
    pub(crate) fn serve(&mut self, ready: &[u64], base: u64, events: &mut Vec<Event>) {
        if ready.contains(&base) {
            match self.acceptor.accept() {
                Ok(conn) if self.unnamed.len() >= MAX_UNNAMED => {
                    say!("soundserver: refusing an output connection, {MAX_UNNAMED} are unnamed already");
                    drop(conn);
                }
                Ok(conn) => self.unnamed.push(Driver { conn, rx: FrameRx::new() }),
                Err(e) => say!("soundserver: output accept failed: {e:?}"),
            }
        }
        if ready.contains(&(base + 1)) {
            self.serve_control(events);
        }
        let unnamed = core::mem::take(&mut self.unnamed);
        for (token, mut driver) in (base + 2..).zip(unnamed) {
            if !ready.contains(&token) {
                self.unnamed.push(driver);
                continue;
            }
            match first_frame(&mut driver) {
                None => self.unnamed.push(driver),
                Some(Err(why)) => say!("soundserver: an output connection dropped: {why}"),
                Some(Ok(FromDriver::Offer(offer))) => self.offered(driver, offer),
                Some(Ok(FromDriver::Stream)) => self.streamed(driver),
                Some(Ok(_)) => say!("soundserver: an output connection opened on a frame that opens nothing"),
            }
        }
    }

    fn offered(&mut self, driver: Driver, offer: Offer) {
        if !matches!(self.phase, Phase::Open) {
            say!("soundserver: refusing an output offer ({offer:?}): it plays to another");
            let _ = driver.conn.try_send_bytes(MSG_REFUSED, &[]);
            return;
        }
        let shape = toyos_mixer::period_frames(
            offer.periods as usize,
            offer.channels as u16,
            offer.period_bytes as usize,
            offer.rate,
        );
        let period_frames = match shape {
            Ok(frames) => frames,
            Err(why) => {
                say!("soundserver: refusing an output offer ({offer:?}): {why}");
                let _ = driver.conn.try_send_bytes(MSG_REFUSED, &[]);
                return;
            }
        };
        let lent = SharedMemory::create_for_device(offer.ring_bytes()).and_then(|ring| Ok((ring.share()?, ring)));
        let (handle, ring) = match lent {
            Ok(lent) => lent,
            Err(why) => {
                say!("soundserver: refusing an output offer: no ring for it ({why:?})");
                let _ = driver.conn.try_send_bytes(MSG_REFUSED, &[]);
                return;
            }
        };
        if let Err(why) = driver.conn.try_signal_with(MSG_ACCEPTED, [handle]) {
            say!("soundserver: an output offer's driver took no acceptance ({why:?})");
            return;
        }
        say!(
            "soundserver: accepted an output: {} Hz {} ch, {} periods of {} bytes, master {}",
            offer.rate,
            offer.channels,
            offer.periods,
            offer.period_bytes,
            match offer.master_bottom {
                Some(bottom) => format!("of {} positions", bottom as u32 + 1),
                None => String::from("none"),
            },
        );
        self.phase = Phase::Accepted { control: driver, offer, period_frames, ring };
    }

    fn streamed(&mut self, stream: Driver) {
        match core::mem::replace(&mut self.phase, Phase::Open) {
            Phase::Accepted { control, offer, period_frames, ring } => {
                let backend = DriverBackend {
                    stream: stream.conn,
                    rx: FrameRx::new(),
                    ring,
                    periods: offer.periods as usize,
                    period_bytes: offer.period_bytes as usize,
                    running: false,
                    run: 0,
                    ended: None,
                };
                let attached = Box::new(Attached { backend, offer, period_frames, generation: 0 });
                self.phase = Phase::Ready { control, attached };
            }
            other => {
                self.phase = other;
                say!("soundserver: a stream connection for no accepted output dropped");
            }
        }
    }

    fn serve_control(&mut self, events: &mut Vec<Event>) {
        let playing = matches!(self.phase, Phase::Playing(..));
        let control = match &mut self.phase {
            Phase::Accepted { control, .. } | Phase::Ready { control, .. } | Phase::Playing(Some(control), _) => control,
            Phase::Open | Phase::Playing(None, _) => return,
        };
        loop {
            let (msg_type, len) = match control.rx.pump(&control.conn) {
                RxStep::Idle => return,
                RxStep::Frame { msg_type, payload_len, .. } => (msg_type, payload_len),
                RxStep::Eof => return self.drop_driver("it hung up".into(), events),
                RxStep::Malformed => {
                    return self.drop_driver("it sent a frame this protocol cannot describe".into(), events);
                }
            };
            match FromDriver::decode(msg_type, control.rx.payload(len)) {
                Some(FromDriver::Master(master)) if playing => events.push(Event::Master(Some(master))),
                Some(FromDriver::MasterUnavailable) if playing => events.push(Event::Master(None)),
                _ => return self.drop_driver(format!("it sent control frame {msg_type}, which this state has not"), events),
            }
        }
    }

    /// Drop the driver this phase holds; where it was played to, the control
    /// thread is told it is [`Event::Gone`].
    fn drop_driver(&mut self, why: String, events: &mut Vec<Event>) {
        match core::mem::replace(&mut self.phase, Phase::Open) {
            Phase::Playing(Some(_), _) => events.push(Event::Gone(why)),
            Phase::Accepted { .. } | Phase::Ready { .. } => {
                say!("soundserver: an accepted output's driver dropped before it was played to: {why}");
            }
            other => self.phase = other,
        }
    }

    /// The mix thread found the driver played to gone: drop it here too.
    pub(crate) fn ended(&mut self, events: &mut Vec<Event>) {
        if matches!(self.phase, Phase::Playing(Some(_), _)) {
            self.drop_driver("the mix thread ended its device loop".into(), events);
        }
    }

    /// The output to begin playing to, now that no stream is open.
    pub(crate) fn attach(&mut self, generation: u32) -> Option<Box<Attached>> {
        if !matches!(self.phase, Phase::Ready { .. }) {
            return None;
        }
        let Phase::Ready { control, mut attached } = core::mem::replace(&mut self.phase, Phase::Open) else {
            unreachable!("matched as ready");
        };
        self.phase = Phase::Playing(Some(control), attached.offer.master_bottom.is_some());
        attached.generation = generation;
        Some(attached)
    }

    /// Forward one master request to the output played to, if it has one.
    pub(crate) fn ask(&mut self, step: Option<i32>) -> Asked {
        let Phase::Playing(Some(control), true) = &self.phase else { return Asked::Unavailable };
        asked(match step {
            Some(delta) => control.conn.try_send_bytes(MSG_MASTER_STEP, &delta.to_le_bytes()),
            None => control.conn.try_send_bytes(MSG_MASTER_MUTE, &[]),
        })
    }
}

/// The first frame of a connection that has said nothing yet, once it is whole.
fn first_frame(driver: &mut Driver) -> Option<Result<FromDriver, &'static str>> {
    match driver.rx.pump(&driver.conn) {
        RxStep::Idle => None,
        RxStep::Eof => Some(Err("it hung up before its first frame")),
        RxStep::Malformed => Some(Err("its first frame is one this protocol cannot describe")),
        RxStep::Frame { msg_type, payload_len, .. } => Some(
            FromDriver::decode(msg_type, driver.rx.payload(payload_len)).ok_or("its first frame is no message"),
        ),
    }
}

/// `played` after `earlier`, as one completion of a `periods`-deep ring: in
/// play order, so the later report's time clocks every period of the earlier
/// one too.
///
/// **A reader a ring behind holds reports that overlap**, `{0..6}` then
/// `{7, 0..5}`: the engine has been round since the first, so the union places
/// it nowhere. That is a whole lap, every period, which the mix loop counts as
/// the drain it is — never a protocol error.
fn combine(earlier: Option<Completion>, played: Played, periods: usize) -> Completion {
    let Some(earlier) = earlier else {
        return Completion {
            mask: played.mask,
            first_nanos: played.first_nanos,
            last_nanos: played.last_nanos,
            clocked: played.clocked,
        };
    };
    let union = earlier.mask | played.mask;
    if earlier.mask & played.mask != 0 || run(union, periods).is_none() {
        let lap = (1u32 << periods) - 1;
        return Completion {
            mask: lap,
            first_nanos: earlier.first_nanos,
            last_nanos: played.last_nanos,
            clocked: periods as u32,
        };
    }
    Completion {
        mask: union,
        first_nanos: earlier.first_nanos,
        last_nanos: played.last_nanos,
        clocked: earlier.mask.count_ones() + played.clocked,
    }
}

/// The output a driver drives, as the mix thread plays to it: the ring
/// soundserver lent, and the stream connection.
///
/// **Nothing the driver sends ends soundserver.** A frame this protocol does
/// not have, a report that is no run of the ring, or a hang-up ends the device
/// loop ([`Backend::ended`]); soundserver is the null sink again.
pub(crate) struct DriverBackend {
    stream: Connection,
    rx: FrameRx<{ Played::BYTES }>,
    ring: SharedMemory,
    periods: usize,
    period_bytes: usize,
    running: bool,
    /// The engine run the last start began, which a report has to carry.
    run: u32,
    ended: Option<String>,
}

impl DriverBackend {
    fn engine(&mut self, msg_type: u32, payload: &[u8]) {
        if let Err(why) = self.stream.try_send_bytes(msg_type, payload) {
            let what = if msg_type == MSG_START { "start" } else { "stop" };
            self.end(format!("the output driver took no {what} ({why:?})"));
        }
    }

    fn end(&mut self, why: String) {
        self.ended.get_or_insert(why);
    }
}

impl Backend for DriverBackend {
    /// The protocol's ring is cyclic: the driver's engine plays it unaided.
    fn pipeline(&self) -> Pipeline {
        Pipeline::Ring
    }

    fn handle(&self) -> toyos::BorrowedHandle<'_> {
        self.stream.as_handle()
    }

    fn period(&self, idx: usize) -> Bytes<'_> {
        assert!(idx < self.periods, "soundserver: the output has no period {idx}");
        self.ring.bytes().sub(idx * self.period_bytes, self.period_bytes)
    }

    /// Every report the driver has sent, as one ([`combine`]). One of another
    /// run than the engine's now, or read while it is stopped, is a period a
    /// stop overtook, and is dropped: a stopped ring is soundserver's whole.
    fn completion(&mut self) -> Option<Completion> {
        let mut out: Option<Completion> = None;
        while self.ended.is_none() {
            let len = match self.rx.pump(&self.stream) {
                RxStep::Idle => break,
                RxStep::Frame { msg_type: MSG_PLAYED, payload_len, .. } => payload_len,
                RxStep::Frame { msg_type, .. } => {
                    self.end(format!("the output driver sent stream frame {msg_type}, which this protocol has not"));
                    break;
                }
                RxStep::Eof => {
                    self.end("the output driver hung up its stream".into());
                    break;
                }
                RxStep::Malformed => {
                    self.end("the output driver sent a stream frame this protocol cannot describe".into());
                    break;
                }
            };
            let played = toyos_sound_output::played(MSG_PLAYED, self.rx.payload(len));
            match played.filter(|played| run(played.mask, self.periods).is_some()) {
                Some(played) if self.running && played.run == self.run => {
                    out = Some(combine(out, played, self.periods));
                }
                Some(_) => {}
                None => self.end(format!("the output driver reported no run of its {}-period ring", self.periods)),
            }
        }
        if self.ended.is_some() { None } else { out }
    }

    /// A cyclic engine replays a period nobody refills: zeroed as it frees, a
    /// late soundserver costs silence.
    fn released(&mut self, idx: usize) {
        self.period(idx).zero();
    }

    fn submit(&mut self, _idx: usize, _bytes: usize) {
        if !self.running {
            self.run = self.run.wrapping_add(1);
            self.engine(MSG_START, &self.run.to_le_bytes());
            self.running = true;
        }
    }

    fn stop(&mut self) {
        if self.running {
            self.engine(MSG_STOP, &[]);
            self.running = false;
        }
    }

    fn ended(&self) -> Option<&str> {
        self.ended.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(mask: u32, at: u64) -> Played {
        Played { mask, clocked: 1, first_nanos: at, last_nanos: at, run: 1 }
    }

    /// Two reports read at one wake: the newer's time clocks the older's
    /// periods and its own first, and the older's first interrupt is the
    /// wake's.
    #[test]
    fn reports_read_together_are_clocked_by_the_newest() {
        let one = combine(None, report(0b0110, 10), 8);
        assert_eq!((one.mask, one.clocked), (0b0110, 1));
        let both = combine(Some(one), report(0b1000, 20), 8);
        assert_eq!((both.mask, both.clocked, both.first_nanos, both.last_nanos), (0b1110, 3, 10, 20));
    }

    /// A reader a ring behind: `{0..6}` then `{7, 0..5}` overlap, a lap then
    /// anything overlaps, and a union that skips a period is no run. Each is a
    /// whole lap, clocked by the newest report, and never a refusal.
    #[test]
    fn reports_a_lagging_reader_holds_are_a_lap() {
        let lap = |c: Completion| (c.mask, c.clocked, c.first_nanos, c.last_nanos);
        let first = combine(None, report(0b0111_1111, 10), 8);
        assert_eq!(lap(combine(Some(first), report(0b1011_1111, 20), 8)), (0xFF, 8, 10, 20));
        let lapped = combine(None, report(0xFF, 10), 8);
        assert_eq!(lap(combine(Some(lapped), report(0b1, 20), 8)), (0xFF, 8, 10, 20));
        let low = combine(None, report(0b0011, 10), 8);
        assert_eq!(lap(combine(Some(low), report(0b11_0000, 20), 8)), (0xFF, 8, 10, 20));
    }
}
