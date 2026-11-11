//! This boot's file on `/log`: a consumer of the replay with an offset of its
//! own, on a thread of its own, as the readers and the console's carriers are.
//!
//! **The loop makes no call on `/log`.** It appends a round to the replay and
//! says so ([`File::grew`]). This thread opens the volume, writes, syncs,
//! rotates and asks a refusing volume again, so a `/log` server that takes a
//! call and never answers holds this thread and nothing else. The stop's
//! flush and mark are asks the loop waits on within a bound and can give up on.
//!
//! **The file is written in batches**: once [`BATCH_BYTES`] are owed, once
//! the oldest owed line has waited [`BATCH_AGE`], and at once for a round
//! holding a line at `Error` or above (a program's panic report is its
//! stderr, a fault the kernel reports an `Alert`) and at the stop's flush.
//! Each batch is written and synced whole.
//!
//! **What a crash costs the file** is the batch not yet written: on a volume
//! that answers, at most [`BATCH_AGE`] of lines below `Error`, which the
//! console was handed as they were read. From the stop's flush until the stop
//! is refused this thread makes no call but the stop mark, so the mark is the
//! volume's last write from this program.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use toyos::say;
use toyos_logstream::{Severity, LOG_OPENED};

use crate::inspect::{Published, State};
use crate::policy::{Retry, LOG_WRITE_BUDGET};
use crate::serve::Hub;
use crate::store::{self, Volume, DIR, MAX_LOG_BYTES, MAX_LOG_PARTS};

/// How long a line below `Error` may wait for the file. Policy, against two
/// costs: each write and sync costs the volume's server, its driver and this
/// program a fixed amount whatever its size, and a spawn says two lines, so a
/// sync per round is that cost per spawn; a machine that dies without
/// stopping loses this much of its file. A second keeps the file a second
/// behind the console at worst and makes one write of everything a busy
/// second says.
pub const BATCH_AGE: Duration = Duration::from_secs(1);

/// How much owed text is written whatever its age. Policy: a flood reaches
/// the file in writes of about this size rather than in one large one a
/// [`BATCH_AGE`], and at this size the fixed cost of a write is small beside
/// what it carries.
pub const BATCH_BYTES: u64 = 64 * 1024;

/// Whether a batch is written at `now`: `owed` bytes, the oldest owed since
/// `since`, of which a line at `Error` or above makes the batch `urgent`.
fn batch_due(owed: u64, since: Option<Instant>, urgent: bool, now: Instant) -> bool {
    owed > 0 && (urgent || owed >= BATCH_BYTES || since.is_some_and(|since| now >= since + BATCH_AGE))
}

/// Whether a round's line at `severity` makes its batch written at once.
pub fn urgent(severity: Severity) -> bool {
    severity >= Severity::Error
}

/// The replay's growth the file's thread has not taken yet, one atomic shared
/// by the two: [`TOLD`] while an [`Ask::Grew`] is in the channel, so any
/// number of rounds is one ask and a file whose `/log` call never returns has
/// no more than that queued; [`URGENT`] for a line among them at `Error` or
/// above.
const TOLD: u8 = 1;
const URGENT: u8 = 2;

enum Ask {
    /// The replay grew: what by is in the shared growth.
    Grew,
    /// The stop: reach the replay's end and sync, stopping the catch-up at
    /// `by`, and answer.
    Finish { by: Instant, answer: SyncSender<()> },
    /// Write the stop mark, unless `by` has passed, and answer whether it is.
    Mark { by: Instant, answer: SyncSender<bool> },
    /// The stop was refused: take back the mark where it was written, and
    /// write what the replay was given since at once.
    Resume { marked: bool },
}

/// The loop's handle on the file's thread.
pub struct File {
    asks: Sender<Ask>,
    growth: Arc<AtomicU8>,
}

impl File {
    /// Start the file's thread, which opens this boot's file under `stem`
    /// (an undated boot's where `None`) and says so with `dated`.
    pub fn start(stem: Option<String>, dated: String, hub: Arc<Hub>, published: Arc<Published>) -> Self {
        let (asks, asked) = mpsc::channel();
        let growth = Arc::new(AtomicU8::new(0));
        let theirs = Arc::clone(&growth);
        std::thread::Builder::new()
            .name("log-file".into())
            .spawn(move || {
                published.publish(None, State::Opening);
                let volume = open(stem, &dated);
                let retry = volume.as_ref().is_some_and(|v| !v.is_open()).then(|| Retry::after(Instant::now()));
                let writer = Writer {
                    volume,
                    hub,
                    published,
                    growth: theirs,
                    retry,
                    degraded: false,
                    owed_since: None,
                    urgent: false,
                    finished: false,
                    stopped: false,
                };
                writer.run(asked);
            })
            .expect("logkeeper: the file's thread could not be started");
        Self { asks, growth }
    }

    /// The replay grew by a round, holding a line written at once where
    /// `urgent`.
    pub fn grew(&self, urgent: bool) {
        let bits = if urgent { TOLD | URGENT } else { TOLD };
        if self.growth.fetch_or(bits, Ordering::AcqRel) & TOLD == 0 {
            self.ask(Ask::Grew);
        }
    }

    /// The stop's flush, its catch-up ended at `by`: whether the file
    /// answered by `until`.
    pub fn finish(&self, by: Instant, until: Instant) -> bool {
        let (answer, answered) = mpsc::sync_channel(1);
        self.ask(Ask::Finish { by, answer });
        answered.recv_timeout(until.saturating_duration_since(Instant::now())).is_ok()
    }

    /// The stop mark, written by `by`: whether it is, or `None` where the
    /// file did not answer by then.
    pub fn mark(&self, by: Instant) -> Option<bool> {
        let (answer, answered) = mpsc::sync_channel(1);
        self.ask(Ask::Mark { by, answer });
        answered.recv_timeout(by.saturating_duration_since(Instant::now())).ok()
    }

    /// The stop was refused, after the mark was written where `marked`.
    pub fn resume(&self, marked: bool) {
        self.ask(Ask::Resume { marked });
    }

    fn ask(&self, ask: Ask) {
        self.asks.send(ask).expect("logkeeper: the file's thread ended");
    }
}

/// This boot's file, opened, and this program's half of the startup report:
/// the kernel says whether it has a console, this whether it has a volume
/// and what its name was decided by.
fn open(stem: Option<String>, dated: &str) -> Option<Volume> {
    let volume = Volume::open(DIR, stem, MAX_LOG_BYTES, |line| say!("{line}"));
    match &volume {
        Some(v) if v.is_open() => say!("{LOG_OPENED}{} ({dated})", v.path()),
        Some(v) => say!(
            "logkeeper: {} is asked again - this boot's lines are kept until {DIR} takes them ({dated})",
            v.path()
        ),
        // `Volume::open` has said why.
        None => say!("logkeeper: this boot's kernel log is on the console only ({dated})"),
    }
    volume
}

struct Writer {
    volume: Option<Volume>,
    hub: Arc<Hub>,
    published: Arc<Published>,
    growth: Arc<AtomicU8>,
    /// When a part the volume refused is next opened again.
    retry: Option<Retry>,
    /// Whether the volume answers, slower than [`LOG_WRITE_BUDGET`] a batch.
    degraded: bool,
    /// When the replay first grew past the file's last batch.
    owed_since: Option<Instant>,
    urgent: bool,
    /// The stop's flush reached the file, which the mark is written over.
    finished: bool,
    /// From the stop's flush until it is refused: no call on `/log` but the mark.
    stopped: bool,
}

impl Writer {
    fn run(mut self, asked: Receiver<Ask>) {
        loop {
            self.publish();
            let ask = match self.next_due() {
                Some(at) => asked.recv_timeout(at.saturating_duration_since(Instant::now())),
                None => asked.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match ask {
                Ok(Ask::Grew) => self.took_growth(),
                Ok(Ask::Finish { by, answer }) => {
                    self.finish(by);
                    // A loop that gave up on the answer has said so.
                    let _ = answer.send(());
                }
                Ok(Ask::Mark { by, answer }) => {
                    let _ = answer.send(self.mark(by));
                }
                Ok(Ask::Resume { marked }) => self.resume(marked),
                Err(RecvTimeoutError::Timeout) => {}
                // The loop never lets go of its handle while the program runs.
                Err(RecvTimeoutError::Disconnected) => return,
            }
            if self.due(Instant::now()) {
                self.write();
            }
        }
    }

    /// The replay's growth since the last [`Ask::Grew`], taken whole: a round
    /// after this one asks again.
    fn took_growth(&mut self) {
        self.urgent |= self.growth.swap(0, Ordering::AcqRel) & URGENT != 0;
        self.owed_since.get_or_insert_with(Instant::now);
    }

    fn publish(&self) {
        let state = match &self.volume {
            None => State::ConsoleOnly,
            Some(v) if !v.is_open() || self.degraded => State::Degraded,
            Some(_) => State::Writing,
        };
        self.published.publish(self.volume.as_ref(), state);
    }

    /// When a batch next falls due with no ask: the oldest owed line's age,
    /// or a refused part's retry. `None` while nothing can be written.
    fn next_due(&self) -> Option<Instant> {
        let v = self.volume.as_ref().filter(|_| !self.stopped)?;
        if !v.is_open() {
            return self.retry.map(|retry| retry.at());
        }
        self.owed_since.map(|since| since + BATCH_AGE)
    }

    fn due(&self, now: Instant) -> bool {
        let Some(v) = self.volume.as_ref().filter(|_| !self.stopped) else { return false };
        if !v.is_open() {
            return self.retry.is_some_and(|retry| retry.due(now));
        }
        batch_due(self.hub.end() - v.through(), self.owed_since, self.urgent, now)
    }

    /// Give the file what the replay owes it, a part's worth, and make it
    /// durable once it has all of it; a part refused is opened again here.
    fn write(&mut self) {
        let began = Instant::now();
        let Some(v) = self.volume.as_mut() else { return };
        let was_open = v.is_open();
        let hub = &self.hub;
        let caught = v.catch_up(
            |at, max| hub.owed(at, max),
            |lost| hub.own_line(Severity::Warn, &format!("logkeeper: {lost} bytes of this boot were let go before {DIR} took them")),
            MAX_LOG_BYTES,
            |line| say!("{line}"),
        );
        match caught {
            Ok(caught) => {
                if !was_open {
                    self.retry = None;
                    say!("logkeeper: {DIR} answers again - {} holds this boot's lines", v.path());
                }
                if !v.is_open() {
                    // A continuation's create was refused.
                    self.retry = Some(Retry::after(Instant::now()));
                } else if caught {
                    (self.owed_since, self.urgent) = (None, false);
                }
                let took = began.elapsed();
                if took > LOG_WRITE_BUDGET && !self.degraded {
                    self.degraded = true;
                    toyos::warn!(
                        "logkeeper: {DIR} answers but slowly (the write took {took:?}) - degraded, nothing lost, \
                         {} is still this boot's log",
                        v.path()
                    );
                } else if took <= LOG_WRITE_BUDGET && self.degraded {
                    self.degraded = false;
                    say!("logkeeper: {DIR} answers at pace again - {}", v.path());
                }
            }
            Err(e) if v.part() >= MAX_LOG_PARTS => {
                toyos::error!("logkeeper: {DIR} would not take a continuation ({e}) - {}", v.path());
                self.volume = None;
            }
            Err(e) => {
                v.lost();
                match self.retry.as_mut() {
                    Some(retry) if !was_open => retry.again(Instant::now()),
                    _ => {
                        toyos::error!(
                            "logkeeper: {DIR} has not answered ({e}) - {} is asked again, and this boot's lines \
                             are kept until it answers",
                            v.path()
                        );
                        self.retry = Some(Retry::after(Instant::now()));
                    }
                }
            }
        }
    }

    /// The stop: the file given the replay to its end, catching up until
    /// `by`, and made durable with what it ends short by said in it.
    fn finish(&mut self, by: Instant) {
        self.stopped = true;
        self.finished = false;
        (self.owed_since, self.urgent) = (None, false);
        if Instant::now() >= by {
            toyos::error!("logkeeper: {DIR} took the stop's flush after its time - the file is as its last batch left it");
            return;
        }
        let Some(v) = self.volume.as_mut() else { return };
        let hub = &self.hub;
        let finished = v.finish(
            |at, max| hub.owed(at, max),
            |lost| hub.own_line(Severity::Warn, &format!("logkeeper: {lost} bytes of this boot were let go before {DIR} took them")),
            hub.end(),
            MAX_LOG_BYTES,
            || Instant::now() < by,
            |short| {
                let text = format!("logkeeper: this file ends {short} bytes short of the boot: the stop came before {DIR} took them");
                hub.own_line(Severity::Error, &text)
            },
            |line| say!("{line}"),
        );
        let short = hub.end() - v.through();
        match finished {
            Ok(_) if short == 0 => {}
            Ok(_) => toyos::error!("logkeeper: {} ends {short} bytes short of the boot at the stop", v.path()),
            Err(e) => toyos::error!(
                "logkeeper: {DIR} refused the stop's flush ({e}) - {} ends {short} bytes short of the boot",
                v.path()
            ),
        }
        self.finished = v.is_open();
    }

    /// The stop mark, over a file the stop's flush made durable, and before
    /// `by`. Withheld, and no call made, unless the flush reached the file:
    /// `/log`'s server may never have answered, and a part the volume refused
    /// is not this boot's log, so the mark would call a stop clean whose log
    /// is not on the volume.
    fn mark(&mut self, by: Instant) -> bool {
        if !self.finished {
            toyos::warn!("logkeeper: {DIR} does not hold this boot's log, so the stop is not marked - the next boot will not count this one as clean");
            return false;
        }
        if Instant::now() >= by {
            toyos::error!("logkeeper: the stop mark was asked of {DIR} after its time - the next boot will not count this one as clean");
            return false;
        }
        store::mark_stopped()
            .inspect_err(|e| {
                toyos::error!("logkeeper: {DIR} would not take the stop mark ({e}) - the next boot will not count this one as clean")
            })
            .is_ok()
    }

    fn resume(&mut self, marked: bool) {
        if marked && let Err(e) = store::unmark_stopped() {
            toyos::error!("logkeeper: {DIR} would not give back the stop mark ({e}) - a later death of this boot will read as clean");
        }
        (self.stopped, self.finished) = (false, false);
        self.owed_since.get_or_insert_with(Instant::now);
        self.urgent = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A batch is written at its size, at its age, or at once for a line
    /// that matters**, and never for nothing.
    #[test]
    fn a_batch_falls_due_at_its_size_its_age_or_an_urgent_line() {
        let since = Instant::now();
        let young = (since + BATCH_AGE).checked_sub(Duration::from_nanos(1)).expect("a nanosecond after `since`");
        assert!(!batch_due(BATCH_BYTES - 1, Some(since), false, young));
        assert!(batch_due(BATCH_BYTES, Some(since), false, young), "a full batch waits for no age");
        assert!(batch_due(1, Some(since), false, since + BATCH_AGE), "an old line waits no longer");
        assert!(batch_due(1, Some(since), true, since), "an urgent line waits for nothing");
        assert!(!batch_due(0, Some(since), true, since + BATCH_AGE), "nothing owed is no write");
        assert!(urgent(Severity::Error) && urgent(Severity::Alert) && !urgent(Severity::Warn));
    }

    fn writer(dir: &str) -> Writer {
        let volume = Volume::open(dir, Some("2026-11-11-120000".into()), MAX_LOG_BYTES, |_| {});
        Writer {
            volume,
            hub: Arc::new(Hub::start(1 << 20, None)),
            published: Arc::new(Published::new()),
            growth: Arc::new(AtomicU8::new(0)),
            retry: None,
            degraded: false,
            owed_since: None,
            urgent: false,
            finished: false,
            stopped: false,
        }
    }

    fn grew(w: &mut Writer, text: &[u8], urgent: bool, at: Instant) {
        w.hub.append(text, None);
        w.owed_since.get_or_insert(at);
        w.urgent |= urgent;
    }

    fn on_disk(w: &Writer) -> Vec<u8> {
        std::fs::read(w.volume.as_ref().unwrap().path()).unwrap()
    }

    /// **The replay reaches the file a batch at a time**, all of it at the
    /// batch's age or at once for a line that matters.
    #[test]
    fn the_file_takes_the_replay_a_batch_at_a_time() {
        let tmp = toyos_tmpdir::TempDir::new("logkeeper-file");
        let mut w = writer(tmp.path().to_str().unwrap());
        let t = Instant::now();
        grew(&mut w, b"one\n", false, t);
        grew(&mut w, b"two\n", false, t + BATCH_AGE / 2);
        assert!(!w.due(t + BATCH_AGE / 2));
        assert!(w.due(t + BATCH_AGE), "the first line's age is the batch's");
        w.write();
        assert_eq!(on_disk(&w), b"one\ntwo\n");
        assert!(w.owed_since.is_none() && w.next_due().is_none(), "a file with everything waits on nothing");
        grew(&mut w, b"panicked\n", true, t + BATCH_AGE);
        assert!(w.due(t + BATCH_AGE));
        w.write();
        assert_eq!(on_disk(&w), b"one\ntwo\npanicked\n");
    }

    /// **Any number of rounds is one ask in the channel** until the file's
    /// thread takes it, with an urgent line among them kept: a file whose
    /// `/log` call never returns has no growing queue behind it.
    #[test]
    fn rounds_the_file_has_not_taken_are_one_ask() {
        let tmp = toyos_tmpdir::TempDir::new("logkeeper-file");
        let mut w = writer(tmp.path().to_str().unwrap());
        let (asks, asked) = mpsc::channel();
        let file = File { asks, growth: Arc::clone(&w.growth) };
        for round in 0..100 {
            file.grew(round == 50);
        }
        assert_eq!(asked.try_iter().count(), 1);
        w.took_growth();
        assert!(w.urgent && w.owed_since.is_some(), "the urgent round among them is kept");
        file.grew(false);
        assert_eq!(asked.try_iter().count(), 1, "a round after the take asks again");
    }

    /// **From the stop's flush until it is refused the file takes no call**,
    /// so the stop mark is the last write; and the mark is never asked of a
    /// file the flush did not reach. A refused stop's lines are written at once.
    #[test]
    fn nothing_reaches_the_file_between_the_stops_flush_and_its_refusal() {
        let tmp = toyos_tmpdir::TempDir::new("logkeeper-file");
        let mut w = writer(tmp.path().to_str().unwrap());
        let t = Instant::now();
        assert!(!w.mark(t + BATCH_AGE), "no flush, no mark");
        grew(&mut w, b"before\n", false, t);
        w.finish(Instant::now() + BATCH_AGE);
        assert!(w.finished);
        assert_eq!(on_disk(&w), b"before\n");
        grew(&mut w, b"after\n", true, t);
        w.volume.as_mut().unwrap().lost();
        w.retry = Some(Retry::after(t));
        assert!(!w.due(t + Duration::from_secs(3600)) && w.next_due().is_none(), "a stopped file is written nothing");
        w.resume(false);
        let retry = w.retry.unwrap().at();
        assert_eq!(w.next_due(), Some(retry), "a refused part is asked again at its retry");
        assert!(w.due(retry));
        w.write();
        assert_eq!(on_disk(&w), b"before\nafter\n");
    }
}
