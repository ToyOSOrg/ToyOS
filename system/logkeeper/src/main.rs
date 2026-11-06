//! `/system/bin/logkeeper` — the machine's log, written to a file by a process that can be
//! killed without taking the kernel with it.
//!
//! The kernel keeps the record ring, writes the console only before userland
//! runs and from the machine's stop, and writes no file; every policy about
//! where records go — what the files are called, how many there are, when they
//! are made durable, what happens when the stick stops answering, who may read
//! the log as it is written, which channel carries the console — is here.
//!
//! # What goes in the log
//!
//! Two kinds of writer, told apart by the head this program gives each line
//! (`toyos_logstream`'s header is the form):
//!
//! - **the kernel's records**, read off its ring with `SYS_LOG_READ`;
//! - **every program's records**, read off the log ring the supervisor made
//!   for it and sent here with the manifest's name for the program and its pid,
//!   on a connection only the supervisor holds ([`toyos_logstream::ORIGINS`]). A record is
//!   that program's because it came out of that ring ([`origin`]). This
//!   program's own lines are records in its own ring, read the same way.
//!
//! Writing a record never waits and never makes a syscall, so this program is
//! nobody's writer's bottleneck: a program that outruns it fills its own ring
//! and the ring counts what it refused, which this program says. A program
//! past its allowance ([`origin::ALLOWANCE`]) has the rest counted and said
//! once a second, so no program can flood the volume or the console.
//!
//! # Order
//!
//! Every record carries the time its writer stamped it with. A round reads
//! every ring, then the kernel's, and writes what is stamped before the round
//! began, in stamp order; what is stamped later waits for the next round,
//! where it is merged with whatever else was stamped before that one began. A
//! record lands out of place only by as long as its writer took between
//! stamping it and publishing it.
//!
//! Rings are read on a cadence ([`POLL_QUICK`] after a round that read any,
//! backing off to [`POLL_SLOW`]); the kernel's readiness and every program's
//! end are events this loop wakes for.
//!
//! # Three sinks, and only one of them is the sink of record
//!
//! The file is. [`serve`]'s readers are the second: the same lines, in the same
//! order, to whoever asks, from the boot's first line however late they ask.
//! The console's carriers are the third ([`carrier`]): the console UART's
//! driver, and a VM's console channel, which the UART goes quiet for while it
//! carries the log. Neither of the two can slow the file: each is a thread
//! with an offset into the replay, which follows the file once it is open.
//!
//! **The volume is opened on a thread of its own, and nothing waits for it**:
//! /log's file server may start late. Until it opens, the readers and the
//! console have each round as it is read; once it opens, the file first takes
//! the boot so far from the replay.
//!
//! # Its whole authority
//!
//! One `SysCap` duplicate carrying `Rights::LOG | Rights::WAIT`, which its
//! manifest row asks for by the name `logread`, and with which it tells the
//! kernel what the console's channels have delivered; the origins acceptor
//! the supervisor endows the row that holds it (`holds = ["log-origins"]`);
//! and what its row adds — the `log` acceptor, where
//! this machine's readers ask for the log and `inspect` asks where it is going
//! ([`inspect`]'s module, which grants nothing), and the `log-carrier` and
//! `log-wire` acceptors, where the console's carriers ask. It claims no device
//! and can name no process. Writing files is ambient — a known residual of the
//! capability endowment, and not this program's to close.
//!
//! # Durability, which is a contract and not a hope
//!
//! **A round's lines are written and the volume made durable before the next
//! round**, and again when the supervisor asks before the machine stops
//! ([`toyos_logstream::FLUSH`]); once the stop has synced its files it asks
//! for the stop mark ([`toyos_logstream::MARK`]). The kernel waits on none of it, so
//! what it says after the flush, at a panic or in the stop, reaches only its
//! console and panel.
//! **A program's line after that flush reaches the UART and is held back from
//! the file**, to a bound ([`STOPPING_BYTES`]): the stop syncs no file still open,
//! so the file ends where the flush made it durable, and a reader of the
//! served log is handed what the file holds and no more. A stop the kernel
//! refused is the supervisor's [`toyos_logstream::RESUME`], and what was held is written
//! then.
//!
//! **A volume that refuses, or whose server ends, is asked again**
//! ([`policy::Retry`], and at the stop's flush whatever the wait), and its
//! file given back what it missed ([`store::Volume`]).

mod carrier;
mod inspect;
mod origin;
mod policy;
mod serve;
mod store;

use std::sync::mpsc::{self, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use toyos::endow::{self, Endowments, SYSCAP_LABEL};
use toyos::ipc::{self, Connection, RxStep};
use toyos::log::{LogTail, Record};
use toyos::poller::{Poller, READABLE};
use toyos::port::Acceptor;
use toyos::say;
use toyos::syscap::SysCap;
use toyos::wake::Bell;
use toyos_logstream::{
    ProgramLine, Registration, Severity, Tag, CARRIER, FLUSH, FLUSHED, LOG_OPENED, MARK, MARKED, MAX_TAG, ORIGINS, REGISTER, RESUME,
    SERVICE, WIRE,
};
use toyos_wallclock::Civil;

use carrier::Carriers;
use serve::Positions;
use origin::{Origin, Said};
use policy::{Retry, Step, LOG_WRITE_BUDGET};
use store::{Volume, DIR, MAX_LOG_BYTES, MAX_LOG_FILES, MAX_LOG_PARTS};

/// Records asked of `SYS_LOG_READ` at once: large enough that an ordinary
/// boot's burst is a handful of syscalls rather than one per line.
const BATCH: usize = 64;

/// The poll's tokens: the kernel's readiness, the supervisor's connection, and
/// each program's end from [`ORIGIN_BASE`] up.
const KERNEL_TOKEN: u64 = 0;
const ORIGINS_TOKEN: u64 = 1;
const ORIGIN_BASE: u64 = 2;

/// Programs whose rings this program reads at once: what one poller can watch
/// beside the other two sources. One past it is refused by name.
const MAX_ORIGINS: usize = Poller::MAX_HANDLES as usize - ORIGIN_BASE as usize;

/// How soon the rings are read again after a round that read any, and the
/// longest the cadence backs off to while none has anything: the latency of a
/// program's line to the file and the console, against wakes on an idle
/// machine.
const POLL_QUICK: Duration = Duration::from_millis(10);
const POLL_SLOW: Duration = Duration::from_millis(250);

/// How much of this boot a reader who connects late can be handed: as much as
/// the volume keeps, so the stream is never the shorter of the two.
const REPLAY_BYTES: usize = MAX_LOG_FILES * MAX_LOG_BYTES as usize;

fn main() {
    let Some(cap) = Endowments::get().take::<SysCap>(SYSCAP_LABEL) else {
        panic!("logkeeper: this program holds no system capability, so it holds no `logread`");
    };
    let Some(acceptor) = Endowments::get().take::<Acceptor>(ORIGINS) else {
        panic!("logkeeper: the supervisor endowed no `{ORIGINS}`, so no program's log can reach this one");
    };
    // The supervisor connected before it started anything, so this is already queued.
    let from_supervisor: Connection = acceptor.accept().expect("logkeeper: the supervisor's origins connection");

    // The wall clock, read once. The kernel reads the RTC once too, so a second
    // reading later in the boot would answer out of the same anchor and tell
    // this program nothing new.
    let (stem, boot_secs, dated) = boot_stamp();

    // Opened off the loop: a request on /log waits for its file server, which
    // the supervisor starts only with storage.
    let (opened, opening) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("log-volume".into())
        .spawn(move || {
            let volume = Volume::open(DIR, stem, MAX_LOG_BYTES, |line| say!("{line}"));
            match &volume {
                // This program's half of the startup report, in one line: the
                // kernel says whether it has a console, this program whether it
                // has a volume and what the name it chose was decided by.
                Some(v) if v.is_open() => say!("{LOG_OPENED}{} ({dated})", v.path()),
                Some(v) => say!(
                    "logkeeper: {} is asked again - this boot's lines are kept until {DIR} takes them ({dated})",
                    v.path()
                ),
                None => say!(
                    "logkeeper: no {DIR} on this machine - this boot's kernel log is on the console only \
                     ({dated})"
                ),
            }
            opened.send(volume).expect("logkeeper: the loop holds the volume's receiver for good");
        })
        .expect("logkeeper: the volume's thread could not be started");

    let hub = Arc::new(serve::Hub::start(REPLAY_BYTES, boot_secs));
    let published = Arc::new(inspect::Published::new());
    if let Some(acceptor) = endow::acceptor(SERVICE) {
        inspect::serve(acceptor, Arc::clone(&published), Arc::clone(&hub));
    }
    let cap = Arc::new(cap);
    let words =
        toyos::log::cursor_words(&cap).unwrap_or_else(|e| panic!("logkeeper: SYS_LOG_READ sized no cursor ({e:?})"));
    // What the kernel put on the UART itself before userland ran, which the
    // UART's carrier is not handed again.
    let shards = words - toyos_abi::log::CURSOR_HEAD_WORDS;
    let spoken =
        toyos::log::spoken(&cap, &vec![0; shards]).unwrap_or_else(|e| panic!("logkeeper: SYS_LOG_SPOKEN refused ({e:?})"));
    let (port, wire) = (endow::acceptor(CARRIER), endow::acceptor(WIRE));
    let carriers =
        (port.is_some() || wire.is_some()).then(|| Carriers::serve(port, wire, Arc::clone(&hub), Arc::clone(&cap)));

    let tail = LogTail::new(vec![0; words]);
    let mut log = Log {
        cap,
        from_supervisor,
        supervisor_rx: ipc::FrameRx::new(),
        carriers,
        spoken: Some(spoken),
        owed_wire: String::new(),
        origins: Vec::new(),
        tail,
        records: vec![Record::EMPTY; BATCH],
        lost: 0,
        waiting: Vec::new(),
        volume: None,
        opening: Some(opening),
        retry: None,
        degraded: false,
        boot_secs,
        hub,
        stopping: None,
    };
    log.run(&published);
}

/// Everything the loop owns.
struct Log {
    cap: Arc<SysCap>,
    from_supervisor: Connection,
    supervisor_rx: ipc::FrameRx<{ 4 + MAX_TAG }>,
    carriers: Option<Arc<Carriers>>,
    /// What the kernel put on the UART itself, a read position per shard,
    /// until the rounds have read past it and said where the UART's carrier
    /// starts.
    spoken: Option<Vec<u64>>,
    /// Those rounds' lines but the kernel's records the UART carried.
    owed_wire: String,
    origins: Vec<Origin>,
    tail: LogTail<Vec<u64>>,
    records: Vec<Record>,
    lost: u64,
    /// Lines stamped after the round they were read in began, which the next
    /// round merges.
    waiting: Vec<Line>,
    volume: Option<Volume>,
    /// The volume's opener, until it answers.
    opening: Option<mpsc::Receiver<Option<Volume>>>,
    /// When a part the volume refused is next opened again.
    retry: Option<Retry>,
    /// Whether the volume answers, slower than `LOG_WRITE_BUDGET` a round.
    degraded: bool,
    boot_secs: Option<u64>,
    hub: Arc<serve::Hub>,
    /// The supervisor's flush was answered and the machine stops: the file's text held
    /// back since, which a refused stop writes, to [`STOPPING_BYTES`].
    stopping: Option<Stopping>,
}

/// The file's text held back after a flush, the lines past the bound, and
/// whether the flush wrote the stop mark.
struct Stopping {
    text: String,
    unwritten: u64,
    marked: bool,
}

/// How long the stop's flush may spend giving the file what it is owed: half
/// the supervisor's bound on the whole flush ([`toyos_quiesce::FLUSH_MS`]),
/// leaving the rest to drain the rings, the final sync and the answer. What
/// it does not reach is said, in the file and on the console.
const STOP_CATCH_UP: Duration = Duration::from_millis(toyos_quiesce::FLUSH_MS / 2);

/// Text a stop may hold back from the file: a refused stop writes it, and
/// what is past it is counted and said then.
const STOPPING_BYTES: usize = 1 << 20;

/// One line on its way out: when it was stamped, and what it is.
struct Line {
    at_ns: u64,
    kind: Kind,
}

enum Kind {
    Kernel(Box<Record>),
    /// A program's record; its origin is named by the tag and pid the supervisor
    /// registered, never by anything in the record.
    Program { tag: Arc<str>, owner: u32, said: Box<Said> },
}

impl Log {
    fn run(&mut self, published: &inspect::Published) -> ! {
        let poller = Poller::new(Poller::MAX_HANDLES);
        let mut cadence = POLL_QUICK;
        // Programs whose end a watch reported, until a round has swept them.
        let mut ended: Vec<usize> = Vec::new();
        loop {
            self.adopt();
            let state = match (&self.volume, self.degraded) {
                (None, _) if self.opening.is_some() => inspect::State::Opening,
                (None, _) => inspect::State::ConsoleOnly,
                (Some(v), _) if !v.is_open() => inspect::State::Degraded,
                (Some(_), true) => inspect::State::Degraded,
                (Some(_), false) => inspect::State::Writing,
            };
            published.publish(self.volume.as_ref(), state, self.tail.lost());

            // **Armed before anything is read**, in the shape every reader of
            // an edge needs: what arrives after a read and before the park has
            // a registration waiting for it. A watch is one-shot, so one that
            // answered here is spent and its source is read below.
            poller.watch(&*self.cap, READABLE, KERNEL_TOKEN);
            poller.watch(&self.from_supervisor, READABLE, ORIGINS_TOKEN);
            for (i, origin) in self.origins.iter().enumerate() {
                poller.watch(&origin.alive, READABLE, ORIGIN_BASE + i as u64);
            }
            let mut spent = false;
            poller.wait(0, 0, |token| {
                spent = true;
                if token >= ORIGIN_BASE {
                    ended.push((token - ORIGIN_BASE) as usize);
                }
            });

            let flush = self.from_supervisor();
            let asked = toyos_abi::clock::stamp_ns();
            let mut read_any = self.round(&mut ended, flush).is_some();
            if flush {
                // Every ring whole up to the flush, whatever a round's bound: a
                // round that read nothing stamped before it is the rings drained
                // of what the supervisor asked for, and a writer that never stops does not
                // hold the flush.
                while self.round(&mut ended, true).is_some_and(|oldest| oldest <= asked) {}
                self.flushed();
                read_any = true;
            }

            cadence = if read_any { POLL_QUICK } else { (cadence * 2).min(POLL_SLOW) };
            if spent || read_any {
                continue;
            }
            // Nothing new: park until the kernel posts, the supervisor speaks, a program
            // ends, or the cadence comes round.
            poller.wait(1, cadence.as_nanos() as u64, |token| {
                if token >= ORIGIN_BASE {
                    ended.push((token - ORIGIN_BASE) as usize);
                }
            });
        }
    }

    /// Take the frames the supervisor has sent, up to a flush: rings, swap words and a
    /// resume. Whether the read ended at a flush.
    ///
    /// **The supervisor is the only peer this connection has**, so a frame that is not
    /// one of these is the supervisor's bug and a loud end. A ring past [`MAX_ORIGINS`]
    /// is refused by name and let go rather than ending this program.
    fn from_supervisor(&mut self) -> bool {
        loop {
            match self.supervisor_rx.pump(&self.from_supervisor) {
                RxStep::Idle => return false,
                RxStep::Eof => panic!("logkeeper: the supervisor closed the origins connection"),
                RxStep::Malformed => {
                    panic!("logkeeper: the supervisor sent a frame the origins protocol cannot carry")
                }
                RxStep::Frame { msg_type: REGISTER, payload_len, .. } => {
                    let payload = self.supervisor_rx.payload(payload_len).to_vec();
                    let Some(registration) = Registration::decode(&payload) else {
                        panic!("logkeeper: the supervisor registered a ring under no name");
                    };
                    let name = registration.tag.as_str();
                    let Some([ring, alive]) = self.supervisor_rx.take_handles::<2>() else {
                        panic!("logkeeper: the supervisor registered {name:?} with no ring");
                    };
                    self.take(registration.tag, registration.pid, ring, Bell::from(alive));
                }
                // **The flush runs before the next frame is read**: the supervisor sends
                // RESUME when the stop it flushed for was refused, which can be
                // before this program got to the flush, and RESUME answers it.
                RxStep::Frame { msg_type: FLUSH, .. } => return true,
                RxStep::Frame { msg_type: MARK, .. } => self.mark(),
                RxStep::Frame { msg_type: RESUME, .. } => self.resume(),
                RxStep::Frame { msg_type, .. } => {
                    panic!("logkeeper: the supervisor sent frame type {msg_type} on the origins connection")
                }
            }
        }
    }

    /// A ring the supervisor registered, read from the next round on.
    fn take(&mut self, tag: Tag<'_>, pid: u32, ring: toyos::OwnedHandle, alive: Bell) {
        if self.origins.len() >= MAX_ORIGINS {
            toyos::warn!(
                "logkeeper: refusing {}'s ring: {MAX_ORIGINS} programs' rings are already read",
                tag.as_str()
            );
            return;
        }
        match Origin::open(tag, pid, ring, alive) {
            Ok(origin) => self.origins.push(origin),
            Err(why) => toyos::error!("logkeeper: refusing a ring the supervisor sent: {why}"),
        }
    }

    /// One round: every ring, then the kernel's records, then everything
    /// stamped before the round began written in stamp order — every line
    /// held, whatever its stamp, where `all` asks it for a flush. A program
    /// whose end `ended` names is swept whole and let go. The oldest stamp any
    /// ring had, or `None` where none had a line.
    fn round(&mut self, ended: &mut Vec<usize>, all: bool) -> Option<u64> {
        let cut = toyos_abi::clock::stamp_ns();
        let mut read: Vec<Said> = Vec::new();
        let mut notes: Vec<String> = Vec::new();
        for (i, origin) in self.origins.iter_mut().enumerate() {
            let counted = origin.read(i, cut, &mut read);
            // The machine stops after a flush: a line begun is said as far as it got.
            if all {
                origin.say_held(&mut read);
            }
            if counted.refused > 0 {
                notes.push(format!(
                    "logkeeper: {} record(s) of {}'s found its ring full and went unwritten",
                    counted.refused, origin.tag
                ));
            }
            if counted.refused_lanes > 0 {
                notes.push(format!(
                    "logkeeper: {} record(s) of {}'s found its lane full and went unwritten",
                    counted.refused_lanes, origin.tag
                ));
            }
            if counted.suppressed > 0 {
                notes.push(format!(
                    "logkeeper: {} record(s) of {}'s past its {} a second went unwritten",
                    counted.suppressed,
                    origin.tag,
                    origin::ALLOWANCE
                ));
            }
            if counted.ahead > 0 {
                notes.push(ahead_note(counted.ahead, &origin.tag));
            }
            if counted.began_suppressing {
                notes.push(format!(
                    "logkeeper: {} is past its {} records a second; the rest this second are counted",
                    origin.tag,
                    origin::ALLOWANCE
                ));
            }
        }
        let oldest = read.iter().map(|said| said.at_ns).min();

        // A program's end, after its reads: what its writers left is swept
        // whole. A ring of a bell nothing rings is the program's own doing and
        // not its end.
        ended.sort_unstable();
        ended.dedup();
        let mut gone: Vec<usize> = Vec::new();
        let count = self.origins.len();
        for &i in ended.iter().filter(|&&i| i < count) {
            let origin = &mut self.origins[i];
            if origin.alive.try_take().is_ok() {
                continue;
            }
            let (abandoned, ahead) = origin.sweep(i, &mut read);
            if ahead > 0 {
                notes.push(ahead_note(ahead, &origin.tag));
            }
            if abandoned > 0 {
                notes.push(format!(
                    "logkeeper: {} left {abandoned} record(s) it had begun and never finished",
                    origin.tag
                ));
            }
            gone.push(i);
        }
        ended.clear();

        let tags: Vec<(Arc<str>, u32)> =
            self.origins.iter().map(|o| (Arc::from(o.tag.as_str()), o.pid)).collect();
        let mut lines: Vec<Line> = std::mem::take(&mut self.waiting);
        lines.extend(read.into_iter().map(|r| {
            let (tag, owner) = &tags[r.origin];
            Line {
                at_ns: r.at_ns,
                kind: Kind::Program { tag: Arc::clone(tag), owner: *owner, said: Box::new(r) },
            }
        }));
        lines.extend(self.kernel_records());
        for &i in gone.iter().rev() {
            self.origins.remove(i);
        }
        for note in notes {
            say!("{note}");
        }
        let (now, later): (Vec<Line>, Vec<Line>) =
            lines.into_iter().partition(|line| all || line.at_ns <= cut);
        let through = written_through(self.tail.positions(), &later);
        self.waiting = later;
        self.write(now, all, through);
        oldest
    }

    /// Whatever the kernel's ring has for this cursor, as lines.
    fn kernel_records(&mut self) -> Vec<Line> {
        let mut out = Vec::new();
        loop {
            let batch = match self.tail.read(&self.cap, &mut self.records) {
                Ok(batch) => batch,
                // The one call this program is built around. A refusal is not
                // survivable by retrying — the buffer and the rights are the
                // same every time — so it ends loudly.
                Err(e) => panic!(
                    "logkeeper: SYS_LOG_READ refused a {BATCH}-record buffer or its cursor ({e:?})"
                ),
            };
            let short = batch.len() < BATCH;
            out.extend(
                batch
                    .iter()
                    .map(|record| Line { at_ns: record.at_ns, kind: Kind::Kernel(Box::new(*record)) }),
            );
            if short {
                break;
            }
        }
        if self.tail.lost() > self.lost {
            // One line per hole rather than one per read.
            toyos::warn!(
                "logkeeper: {} kernel record(s) were overwritten in a shard before this reader got \
                 to them",
                self.tail.lost() - self.lost
            );
            self.lost = self.tail.lost();
        }
        out
    }

    /// The lines, in stamp order, into the file, the readers and the console's
    /// carriers, which are the kernel's log through the read positions
    /// `through`; while the machine stops, held back from the file, and each
    /// program's line to the UART's carrier.
    fn write(&mut self, mut lines: Vec<Line>, flushing: bool, through: Positions) {
        if lines.is_empty() {
            return;
        }
        lines.sort_by_key(|line| line.at_ns);
        let mut file = String::new();
        // What the UART is owed of these lines, while the kernel's own on it
        // are yet to be read past.
        let mut wire = String::new();
        let mut programs = String::new();
        for line in &lines {
            match &line.kind {
                Kind::Kernel(record) => {
                    let text = format!("{}\n", record.dated(&stamp(self.boot_secs, record.at_ns())));
                    if !self.spoken.as_ref().is_some_and(|spoken| on_the_uart(record, spoken)) {
                        wire.push_str(&text);
                    }
                    file.push_str(&text);
                }
                Kind::Program { tag, owner, said } => {
                    let tag = Tag::new(tag).expect("an origin's name is a tag");
                    let pid = (said.pid != *owner).then_some(said.pid);
                    let at = stamp(self.boot_secs, Some(said.at_ns));
                    let line = ProgramLine {
                        stamp: &at,
                        at_ns: said.at_ns,
                        severity: said.severity,
                        tid: said.tid,
                        pid,
                        tag,
                        text: &said.text,
                    };
                    let line = format!("{line}\n");
                    file.push_str(&line);
                    wire.push_str(&line);
                    if self.stopping.is_some() {
                        programs.push_str(&line);
                    }
                }
            }
        }
        if let Some(held) = &mut self.stopping {
            // The kernel's records from the flush on are the stop's to put on
            // the UART; a program's reach it only through its carrier.
            self.hub.append_stopping(programs.as_bytes());
            if held.text.len() + file.len() > STOPPING_BYTES {
                held.unwritten += lines.len() as u64;
            } else {
                held.text.push_str(&file);
            }
            return;
        }
        // The file first: once it is open, a reader is served only what /log
        // already holds, whenever the machine stops between the two.
        self.to_volume(file.as_bytes(), !flushing);
        self.hub.append(file.as_bytes(), Some(Arc::clone(&through)));
        if let Some(spoken) = &self.spoken {
            self.owed_wire.push_str(&wire);
            if through.iter().zip(spoken).all(|(read, spoken)| read >= spoken) {
                self.hub.set_wire_from(self.hub.end(), std::mem::take(&mut self.owed_wire).into_bytes());
                self.spoken = None;
            }
        }
    }

    /// Write a round to the volume, and make it durable unless it is one of a
    /// flush's rounds, which `flushed` makes durable together: a stop's flush
    /// is bounded by the supervisor, and a sync per round of it is what outruns that.
    fn to_volume(&mut self, text: &[u8], sync: bool) {
        if !self.caught_up() {
            return;
        }
        let Some(v) = self.volume.as_mut() else { return };
        let began = Instant::now();
        let mut refused = v.write(text).err().map(|e| (Step::Append, e.to_string()));
        if refused.is_none() && sync {
            refused = self.sync().err();
        }
        // A volume that answered, and took longer than a log is worth doing it.
        if refused.is_none() && began.elapsed() > LOG_WRITE_BUDGET {
            refused = Some((Step::TooSlow, format!("it took {:?}", began.elapsed())));
        }
        self.answered(refused);
    }

    /// The volume, once its opener answers outside a stop. Its file stands
    /// at the replay's start, so the next round's catch-up gives it the boot
    /// so far, a part at a time.
    fn adopt(&mut self) {
        if self.stopping.is_some() {
            return;
        }
        let Some(opening) = &self.opening else { return };
        match opening.try_recv() {
            Ok(volume) => self.took(volume),
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => panic!("logkeeper: the volume's thread ended without answering"),
        }
    }

    /// The opener's answer.
    fn took(&mut self, volume: Option<Volume>) {
        self.opening = None;
        self.volume = volume;
    }

    /// Whether the file has every byte the replay has, so a round's text
    /// goes after them. A part the volume refused is opened again once the
    /// retry is due, and the file is given what it is owed, a part's worth a
    /// round.
    fn caught_up(&mut self) -> bool {
        let now = Instant::now();
        let Some(v) = self.volume.as_mut() else { return false };
        let was_open = v.is_open();
        if was_open && v.through() == self.hub.end() {
            return true;
        }
        if !was_open && !self.retry.get_or_insert(Retry::after(now)).due(now) {
            return false;
        }
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
                    self.retry = Some(Retry::after(now));
                }
                caught && v.is_open()
            }
            Err(e) if v.part() >= MAX_LOG_PARTS => {
                toyos::error!("logkeeper: {DIR} would not take a continuation ({e}) - {}", v.path());
                self.volume = None;
                false
            }
            Err(e) => {
                v.lost();
                match self.retry.as_mut() {
                    Some(retry) if !was_open => retry.again(now),
                    _ => {
                        toyos::error!("logkeeper: {DIR} has not answered ({e}) - {} is asked again", v.path());
                        self.retry = Some(Retry::after(now));
                    }
                }
                false
            }
        }
    }

    /// Make the volume durable now.
    fn sync(&mut self) -> Result<(), (Step, String)> {
        let Some(v) = self.volume.as_mut() else { return Ok(()) };
        v.sync().map_err(|e| (Step::Flush, e.to_string()))
    }

    /// What a round's write came to: rotation after a clean one, and the
    /// give-up policy after a refused one.
    fn answered(&mut self, refused: Option<(Step, String)>) {
        let Some(path) = self.volume.as_ref().map(Volume::path) else { return };
        let Some((step, why)) = refused else {
            if self.degraded {
                self.degraded = false;
                say!("logkeeper: {DIR} answers at pace again - {path}");
            }
            self.rotate_if_full();
            return;
        };
        match step {
            // Say so once, keep the lines in the replay, and ask again.
            Step::Append | Step::Flush => {
                toyos::error!(
                    "logkeeper: {DIR} has not answered ({}: {why}) - {path} is asked again, and this \
                     boot's lines are kept until it answers",
                    step.as_str()
                );
                self.lost();
            }
            // Every call answered, slowly: the round is durable.
            Step::TooSlow => {
                if !self.degraded {
                    self.degraded = true;
                    toyos::warn!(
                        "logkeeper: {DIR} answers but slowly ({}: {why}) - degraded, nothing lost, \
                         {path} is still this boot's log",
                        step.as_str()
                    );
                }
                self.rotate_if_full();
            }
        }
    }

    fn rotate_if_full(&mut self) {
        let Some(v) = self.volume.as_mut() else { return };
        if !v.full() {
            return;
        }
        match v.rotate(|line| say!("{line}")) {
            Ok(()) if v.is_open() => {}
            Ok(()) => self.lost(),
            Err(e) if v.part() >= MAX_LOG_PARTS => {
                toyos::error!("logkeeper: {DIR} would not take a continuation ({e}) - {}", v.path());
                self.volume = None;
            }
            Err(e) => {
                toyos::error!("logkeeper: {DIR} has not answered ({e}) - {} is asked again", v.path());
                self.lost();
            }
        }
    }

    /// The volume refused the part: it is asked again once the retry is due.
    fn lost(&mut self) {
        if let Some(v) = self.volume.as_mut() {
            v.lost();
        }
        self.retry = Some(Retry::after(Instant::now()));
    }

    /// The supervisor asked for the log whole before the machine stops: every line is
    /// written by now, so the volume is made durable, and the supervisor is told.
    fn flushed(&mut self) {
        let deadline = Instant::now() + STOP_CATCH_UP;
        // An opener that answers during the flush's rounds or the stop's own
        // time: the file takes the boot in the catch-up below.
        self.adopt();
        if let Some(opening) = &self.opening {
            match opening.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(volume) => self.took(volume),
                Err(_) => toyos::error!("logkeeper: {DIR} had not opened by the stop - this boot's log is on the console only"),
            }
        }
        if let Some(v) = self.volume.as_mut() {
            let hub = &self.hub;
            let finished = v.finish(
                |at, max| hub.owed(at, max),
                |lost| hub.own_line(Severity::Warn, &format!("logkeeper: {lost} bytes of this boot were let go before {DIR} took them")),
                hub.end(),
                MAX_LOG_BYTES,
                || Instant::now() < deadline,
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
        }
        // The console's carriers take the log up to here, and the UART what
        // the machine says from here as it stops.
        if let Some(carriers) = &self.carriers {
            carriers.stop(self.hub.end());
        }
        self.stopping = Some(Stopping { text: String::new(), unwritten: 0, marked: false });
        if let Err(e) = self.from_supervisor.signal(FLUSHED) {
            panic!("logkeeper: the supervisor could not be told the flush is done: {e:?}");
        }
    }

    /// The supervisor's stop has synced its files and is about to stop its
    /// drivers: write the stop mark over a durable log, and say so. No volume,
    /// no call: `/log`'s server may never have answered.
    fn mark(&mut self) {
        let Some(stopping) = self.stopping.as_mut() else {
            panic!("logkeeper: the supervisor asked for the stop mark, and no stop was flushed for");
        };
        stopping.marked = self.volume.as_ref().is_some_and(Volume::is_open)
            && store::mark_stopped()
                .inspect_err(|e| {
                    toyos::error!("logkeeper: {DIR} would not take the stop mark ({e}) - the next boot will not count this one as clean")
                })
                .is_ok();
        if let Err(e) = self.from_supervisor.signal(MARKED) {
            panic!("logkeeper: the supervisor could not be told the stop is marked: {e:?}");
        }
    }

    /// The stop was refused: what was held back goes to the file, which is
    /// this boot's log again.
    fn resume(&mut self) {
        let Some(held) = self.stopping.take() else {
            panic!("logkeeper: the supervisor said the machine runs on, and no stop was flushed for");
        };
        toyos::warn!("logkeeper: the stop was refused, so {DIR} takes this boot's lines again");
        if held.marked
            && let Err(e) = store::unmark_stopped()
        {
            toyos::error!("logkeeper: {DIR} would not give back the stop mark ({e}) - a later death of this boot will read as clean");
        }
        if held.unwritten > 0 {
            toyos::warn!(
                "logkeeper: {} line(s) said while the machine stopped were past {STOPPING_BYTES} bytes \
                 held back and went unwritten",
                held.unwritten
            );
        }
        self.to_volume(held.text.as_bytes(), true);
        self.hub.resume(held.text.as_bytes());
        if let Some(carriers) = &self.carriers {
            carriers.resume();
        }
    }
}

/// Whether the kernel put `record` on the UART itself, by the read positions
/// it had spoken through.
fn on_the_uart(record: &Record, spoken: &[u64]) -> bool {
    spoken.get(usize::from(record.cpu)).is_some_and(|&through| record.seq < through)
}

/// The kernel's read positions a round's written lines are the log through:
/// the cursor's, each shard's held back to its first record still `waiting`
/// for a later round.
fn written_through(cursor: &[u64], waiting: &[Line]) -> Positions {
    let mut through = cursor.to_vec();
    for line in waiting {
        if let Kind::Kernel(record) = &line.kind
            && let Some(at) = through.get_mut(usize::from(record.cpu))
        {
            *at = (*at).min(record.seq);
        }
    }
    through.into()
}

fn ahead_note(ahead: u64, tag: &str) -> String {
    format!("logkeeper: {ahead} record(s) of {tag}'s were stamped ahead of the clock and are written as read")
}

/// This boot's file stem, and the epoch second its counter started at: where
/// every line's time counts from.
///
/// `None` for the stem is a boot that cannot be placed in time, which takes an
/// `unknown-NN` name.
fn boot_stamp() -> (Option<String>, Option<u64>, String) {
    let Some(secs) = toyos::system::clock_epoch() else {
        return (None, None, "undated: this machine will not say what time it is".into());
    };
    let civil = Civil::from_unix_secs(secs);
    let since_zero = toyos_abi::clock::stamp_ns() / 1_000_000_000;
    (Some(format!("{}", civil.stem())), Some(secs.saturating_sub(since_zero)), format!("{civil} UTC"))
}

/// A line's wall-clock stamp: the second the counter started at, plus the
/// line's own time — which the line carries too, so `/log` holds both
/// clocks. A line with no time has no wall clock either.
pub(crate) fn stamp(boot_secs: Option<u64>, at_ns: Option<u64>) -> String {
    match boot_secs.zip(at_ns) {
        Some((base, at_ns)) => format!("{}", Civil::from_unix_secs(base + at_ns / 1_000_000_000)),
        // An undated boot, or an untimed line, writes the space the stamp would
        // have taken, so the columns line up and nothing has to be re-parsed to
        // notice that a machine had no clock.
        None => "---------- --------".into(),
    }
}

#[cfg(test)]
mod tests {
    /// The `log` port answers a reader and `inspect` on one acceptor, told
    /// apart by the request's frame type alone.
    #[test]
    fn readers_and_inspect_share_one_port_and_never_one_request() {
        assert_eq!(toyos_inspect::LOG.port, toyos_logstream::SERVICE);
        assert_ne!(toyos_logstream::READ, toyos_inspect::MSG_INSPECT);
    }

    fn kernel(cpu: u16, seq: u64) -> super::Line {
        let record = super::Record { cpu, seq, ..super::Record::EMPTY };
        super::Line { at_ns: 0, kind: super::Kind::Kernel(Box::new(record)) }
    }

    /// **A round speaks for the kernel's log only as far as it wrote it**: a
    /// shard's position is held back to its first record still waiting for
    /// a later round, and the others' are the cursor's.
    #[test]
    fn a_round_is_through_the_cursor_but_what_waits() {
        let waiting = [kernel(1, 7), kernel(1, 9)];
        assert_eq!(&*super::written_through(&[5, 12, 3], &waiting), &[5, 7, 3]);
        assert_eq!(&*super::written_through(&[5, 12], &[]), &[5, 12]);
    }

    /// The kernel's records the UART carried before the hand-over are those
    /// before the position it spoke through in their shard, whatever their
    /// stamps say.
    #[test]
    fn what_the_uart_carried_is_told_by_position() {
        let on = |cpu, seq| match kernel(cpu, seq).kind {
            super::Kind::Kernel(record) => super::on_the_uart(&record, &[4, 2]),
            super::Kind::Program { .. } => unreachable!(),
        };
        assert!(on(0, 3));
        assert!(!on(0, 4));
        assert!(on(1, 1));
        assert!(!on(1, 2));
        assert!(!on(2, 0), "a shard it spoke nothing of");
    }
}
