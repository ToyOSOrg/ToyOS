//! `/system/bin/logd` — the machine's log, written to a file by a process that can be
//! killed without taking the kernel with it.
//!
//! The kernel keeps the record ring and the console and writes no file; every
//! policy about where records go — what the files are called, how many there
//! are, when they are made durable, what happens when the stick stops
//! answering, who may read the log as it is written — is here.
//!
//! # What goes in the log
//!
//! Two kinds of writer, told apart by the head this program gives each line
//! (`toyos_logstream`'s header is the form):
//!
//! - **the kernel's records**, read off its ring with `SYS_LOG_READ`;
//! - **every program's records**, read off the log ring `/system/bin/init` made
//!   for it and sent here with the manifest's name for the program and its pid,
//!   on a connection only init holds ([`toyos_logstream::ORIGINS`]). A record is
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
//! The console is the third: each program's line with its head and no wall
//! clock, written on the one console handle init gives this program. The
//! kernel puts its own records on the console, and this program never does.
//! Neither of the two can slow the file: a reader is a thread with an offset
//! into what the file already has, and the console takes only the whole lines
//! it has room for — the rest waits here, bounded ([`CONSOLE_BYTES`]).
//!
//! # Its whole authority
//!
//! One `SysCap` duplicate carrying `Rights::LOG | Rights::WAIT`, which its
//! manifest row asks for by the name `logread`; the origins acceptor and the
//! writable console init endows it; and what its row adds — on a test estate a
//! `netd` connector to serve the network, and the `log` acceptor, where this
//! machine's readers ask for the log and `inspect` asks where it is going
//! ([`inspect`]'s module, which grants nothing). It claims no device and can
//! name no process. Writing files is ambient — a known residual of the
//! capability endowment, and not this program's to close.
//!
//! # Durability, which is a contract and not a hope
//!
//! A round's lines are written; the volume is made durable at an `Alert`, at
//! most [`SYNC_INTERVAL`] after the oldest line not yet durable, at a
//! rotation, and when init asks before the machine stops
//! ([`toyos_logstream::FLUSH`]). The kernel waits on none of it: a panicking
//! kernel's report is in its black box, and so are the stop's own last records.
//!
//! `SYS_FSYNC` reaches the device's own cache flush. **A flush that would block
//! is not a flush that failed**: `io::ErrorKind::WouldBlock` from `sync_all` is
//! `kernel/src/block.rs`'s `BlockError::BudgetExpired`, which means the kernel
//! declined to *start* the operation on the caller's own clock — nothing was
//! issued, the device is untouched, and the bytes are still in the file waiting
//! for the next flush. `policy::fate` is the whole decision, and `policy`'s own
//! header is the argument.

mod inspect;
mod origin;
mod policy;
mod serve;
mod store;
mod wall;

use std::sync::Arc;
use std::time::{Duration, Instant};

use toyos::endow::{self, Endowments, SYSCAP_LABEL};
use toyos::ipc::{self, Connection, RxStep};
use toyos::log::{LogTail, Record, Severity};
use toyos::poller::{Poller, READABLE, WRITABLE};
use toyos::port::Acceptor;
use toyos::say;
use toyos::syscap::SysCap;
use toyos::{Console, Pipe};
use toyos_abi::syscall::SyscallError;
use toyos_logstream::{
    ProgramLine, Registration, Tag, CONSOLE, FLUSH, FLUSHED, MAX_TAG, ORIGINS, REGISTER, SERVICE,
    SWAP, SWAP_BACK, SWAP_LEAVING,
};
use toyos_wallclock::Civil;

use origin::{Origin, Said};
use policy::{fate, Fate, Step, LOG_WRITE_BUDGET};
use store::{Volume, DIR, MAX_LOG_BYTES, MAX_LOG_FILES, ROTATE_FAST_BYTES};

/// Records asked of `SYS_LOG_READ` at once: above `MAX_LOG_SHARDS`, which the
/// call refuses below, and large enough that an ordinary boot's burst is a
/// handful of syscalls rather than one per line.
const BATCH: usize = 64;

/// The poll's tokens: the kernel's readiness, init's connection, the console's
/// room, and each program's end from [`ORIGIN_BASE`] up.
const KERNEL_TOKEN: u64 = 0;
const ORIGINS_TOKEN: u64 = 1;
const CONSOLE_TOKEN: u64 = 2;
const ORIGIN_BASE: u64 = 3;

/// Programs whose rings this program reads at once: what one poller can watch
/// beside the other three sources. One past it is refused by name.
const MAX_ORIGINS: usize = Poller::MAX_HANDLES as usize - ORIGIN_BASE as usize;

/// How soon the rings are read again after a round that read any, and the
/// longest the cadence backs off to while none has anything: the latency of a
/// program's line to the file and the console, against wakes on an idle
/// machine.
const POLL_QUICK: Duration = Duration::from_millis(10);
const POLL_SLOW: Duration = Duration::from_millis(250);

/// The longest a written line waits for the volume to be made durable.
///
/// **A policy number**: what it trades is a device flush per round — the
/// worst workload FAT has — against how much a machine that loses power
/// loses. The crash that matters is the kernel's, and its tail is in the black
/// box.
const SYNC_INTERVAL: Duration = Duration::from_secs(2);

/// How much of this boot a reader who connects late can be handed: as much as
/// the volume keeps, so the stream is never the shorter of the two.
const REPLAY_BYTES: usize = MAX_LOG_FILES * MAX_LOG_BYTES as usize;

/// Program lines the console may fall behind the log by. Past it whole lines
/// are counted and said, and `/log` has them.
const CONSOLE_BYTES: usize = 1 << 20;

fn main() {
    let Some(cap) = Endowments::get().take::<SysCap>(SYSCAP_LABEL) else {
        panic!("logd: this program holds no system capability, so it holds no `logread`");
    };
    let Some(acceptor) = Endowments::get().take::<Acceptor>(ORIGINS) else {
        panic!("logd: init endowed no `{ORIGINS}`, so no program's log can reach this one");
    };
    let Some(console) = Endowments::get().take::<Console>(CONSOLE) else {
        panic!("logd: init endowed no `{CONSOLE}`, so no program's line can reach the console");
    };
    // init connected before it started anything, so this is already queued.
    let from_init: Connection = acceptor.accept().expect("logd: init's origins connection");

    let rotate_at = if std::env::args().any(|a| a == "--rotate-fast") {
        ROTATE_FAST_BYTES
    } else {
        MAX_LOG_BYTES
    };

    // The wall clock, read once. The kernel reads the RTC once too, so a second
    // reading later in the boot would answer out of the same anchor and tell
    // this program nothing new.
    let (stem, boot_local, zone) = boot_stamp();

    let volume = Volume::open(stem, rotate_at, |line| say!("{line}"));
    match &volume {
        // This program's half of the startup report, in one line: the kernel
        // says whether it has a console, this program whether it has a volume
        // and what the name it chose was decided by.
        Some(v) => say!("logd: this boot's kernel log is {} ({zone})", v.path()),
        None => say!(
            "logd: no {DIR} on this machine - this boot's kernel log is on the console only \
             ({zone})"
        ),
    }

    let hub = Arc::new(serve::Hub::start(REPLAY_BYTES, boot_local));
    let published = Arc::new(inspect::Published::new(hub.network()));
    if let Some(acceptor) = endow::acceptor(SERVICE) {
        inspect::serve(acceptor, Arc::clone(&published), Arc::clone(&hub));
    }

    let mut log = Log {
        cap,
        from_init,
        init_rx: ipc::FrameRx::new(),
        console,
        console_held: Vec::new(),
        console_unshown: 0,
        origins: Vec::new(),
        tail: LogTail::new(),
        records: vec![Record::EMPTY; BATCH],
        lost: 0,
        waiting: Vec::new(),
        volume,
        unsynced_since: None,
        alert_unsynced: false,
        retrying_since: None,
        degraded: false,
        boot_local,
        hub,
        stall: Stall::from_args(),
    };
    log.run(&published);
}

/// Everything the loop owns.
struct Log {
    cap: SysCap,
    from_init: Connection,
    init_rx: ipc::FrameRx<{ 4 + MAX_TAG }>,
    console: Console,
    /// Program lines the console has not taken yet, whole lines only.
    console_held: Vec<u8>,
    console_unshown: u64,
    origins: Vec<Origin>,
    tail: LogTail,
    records: Vec<Record>,
    lost: u64,
    /// Lines stamped after the round they were read in began, which the next
    /// round merges.
    waiting: Vec<Line>,
    volume: Option<Volume>,
    /// When the oldest line not yet made durable was written.
    unsynced_since: Option<Instant>,
    /// Whether an `Alert` was written and not yet made durable.
    alert_unsynced: bool,
    /// When the current run of consecutive refused rounds began.
    retrying_since: Option<Instant>,
    /// Whether the volume answers, slower than `LOG_WRITE_BUDGET` a round.
    degraded: bool,
    boot_local: Option<u64>,
    hub: Arc<serve::Hub>,
    stall: Option<Stall>,
}

/// One line on its way out: when it was stamped, and what it is.
struct Line {
    at_ns: u64,
    kind: Kind,
}

enum Kind {
    Kernel(Box<Record>),
    /// A program's record; its origin is named by the tag and pid init
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
            let state = match (&self.volume, self.retrying_since, self.degraded) {
                (None, _, _) => inspect::State::ConsoleOnly,
                (Some(_), Some(_), _) => inspect::State::Retrying,
                (Some(_), None, true) => inspect::State::Degraded,
                (Some(_), None, false) => inspect::State::Writing,
            };
            published.publish(self.volume.as_ref(), state, self.tail.lost());

            // **Armed before anything is read**, in the shape every reader of
            // an edge needs: what arrives after a read and before the park has
            // a registration waiting for it. A watch is one-shot, so one that
            // answered here is spent and its source is read below.
            poller.watch(&self.cap, READABLE, KERNEL_TOKEN);
            poller.watch(&self.from_init, READABLE, ORIGINS_TOKEN);
            if !self.console_held.is_empty() {
                poller.watch(&self.console, WRITABLE, CONSOLE_TOKEN);
            }
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

            let flush = self.from_init();
            let asked = toyos_abi::clock::nanos_since_boot();
            let mut read_any = self.round(&mut ended, flush).is_some();
            if flush {
                // Every ring whole up to the flush, whatever a round's bound: a
                // round that read nothing stamped before it is the rings drained
                // of what init asked for, and a writer that never stops does not
                // hold the flush.
                while self.round(&mut ended, true).is_some_and(|oldest| oldest <= asked) {}
                self.flushed();
                read_any = true;
            }
            self.sync_if_due();
            self.feed_console();

            cadence = if read_any { POLL_QUICK } else { (cadence * 2).min(POLL_SLOW) };
            if spent || read_any {
                continue;
            }
            // Nothing new: park until the kernel posts, init speaks, a program
            // ends, the console has room, the cadence comes round, or the
            // volume is owed its sync.
            let mut wait = cadence;
            if let Some(since) = self.unsynced_since {
                wait = wait.min(SYNC_INTERVAL.saturating_sub(since.elapsed()));
            }
            poller.wait(1, wait.as_nanos().max(1) as u64, |token| {
                if token >= ORIGIN_BASE {
                    ended.push((token - ORIGIN_BASE) as usize);
                }
            });
        }
    }

    /// Take every frame init has sent: rings, swap words, and a flush.
    /// Whether a flush was asked for.
    ///
    /// **init is the only peer this connection has**, so a frame that is not
    /// one of these is init's bug and a loud end. A ring past [`MAX_ORIGINS`]
    /// is refused by name and let go rather than ending this program.
    fn from_init(&mut self) -> bool {
        let mut flush = false;
        loop {
            match self.init_rx.pump(&self.from_init) {
                RxStep::Idle => return flush,
                RxStep::Eof => panic!("logd: init closed the origins connection"),
                RxStep::Malformed => {
                    panic!("logd: init sent a frame the origins protocol cannot carry")
                }
                RxStep::Frame { msg_type: REGISTER, payload_len } => {
                    let payload = self.init_rx.payload(payload_len).to_vec();
                    let Some(registration) = Registration::decode(&payload) else {
                        panic!("logd: init registered a ring under no name");
                    };
                    let name = registration.tag.as_str();
                    let Some([ring, alive]) = self.from_init.recv_handles_exact::<2>() else {
                        panic!("logd: init registered {name:?} with no ring");
                    };
                    // SAFETY: the kernel moved this handle into this table with
                    // the frame that names it, and nothing else answers for it.
                    let alive = unsafe { Pipe::from_raw(alive) };
                    if self.origins.len() >= MAX_ORIGINS {
                        toyos::warn!(
                            "logd: refusing {name}'s ring: {MAX_ORIGINS} programs' rings are \
                             already read"
                        );
                        toyos_abi::syscall::close(ring);
                        continue;
                    }
                    match Origin::open(registration.tag, registration.pid, ring, alive) {
                        Ok(origin) => self.origins.push(origin),
                        Err(why) => toyos::error!("logd: refusing a ring init sent: {why}"),
                    }
                }
                RxStep::Frame { msg_type: SWAP, payload_len } => {
                    let word = match self.init_rx.payload(payload_len) {
                        [SWAP_LEAVING] => serve::Carrier::Leaving,
                        [SWAP_BACK] => serve::Carrier::Back,
                        other => panic!("logd: init sent a swap word {other:?}"),
                    };
                    self.hub.carrier(word);
                }
                RxStep::Frame { msg_type: FLUSH, .. } => flush = true,
                RxStep::Frame { msg_type, .. } => {
                    panic!("logd: init sent frame type {msg_type} on the origins connection")
                }
            }
        }
    }

    /// One round: every ring, then the kernel's records, then everything
    /// stamped before the round began written in stamp order — every line
    /// held, whatever its stamp, where `all` asks it for a flush. A program
    /// whose end `ended` names is swept whole and let go. The oldest stamp any
    /// ring had, or `None` where none had a line.
    fn round(&mut self, ended: &mut Vec<usize>, all: bool) -> Option<u64> {
        let cut = toyos_abi::clock::nanos_since_boot();
        let mut read: Vec<Said> = Vec::new();
        let mut notes: Vec<String> = Vec::new();
        for (i, origin) in self.origins.iter_mut().enumerate() {
            if self.stall.as_ref().is_some_and(|s| s.origin == origin.tag) {
                continue;
            }
            let counted = origin.read(i, cut, &mut read);
            // The machine stops after a flush: a line begun is said as far as it got.
            if all {
                origin.say_held(&mut read);
            }
            if counted.refused > 0 {
                notes.push(format!(
                    "logd: {} record(s) of {}'s found its ring full and went unwritten",
                    counted.refused, origin.tag
                ));
            }
            if counted.refused_lanes > 0 {
                notes.push(format!(
                    "logd: {} record(s) of {}'s found its lane full and went unwritten",
                    counted.refused_lanes, origin.tag
                ));
            }
            if counted.suppressed > 0 {
                notes.push(format!(
                    "logd: {} record(s) of {}'s past its {} a second went unwritten",
                    counted.suppressed,
                    origin.tag,
                    origin::ALLOWANCE
                ));
            }
            if counted.began_suppressing {
                notes.push(format!(
                    "logd: {} is past its {} records a second; the rest this second are counted",
                    origin.tag,
                    origin::ALLOWANCE
                ));
            }
        }
        let oldest = read.iter().map(|said| said.at_ns).min();

        // A program's end, after its reads: what its writers left is swept
        // whole. A byte on a pipe nothing writes is the program's own doing
        // and not its end.
        ended.sort_unstable();
        ended.dedup();
        let mut gone: Vec<usize> = Vec::new();
        let count = self.origins.len();
        for &i in ended.iter().filter(|&&i| i < count) {
            let origin = &mut self.origins[i];
            let mut probe = [0u8; 1];
            match origin.alive.read_nonblock(&mut probe) {
                Ok(0) => {}
                Ok(_) | Err(SyscallError::WouldBlock) => continue,
                Err(e) => panic!("logd: {}'s end refused a read: {e:?}", origin.tag),
            }
            let abandoned = origin.sweep(i, &mut read);
            if abandoned > 0 {
                notes.push(format!(
                    "logd: {} left {abandoned} record(s) it had begun and never finished",
                    origin.tag
                ));
            }
            gone.push(i);
        }
        ended.clear();
        self.release_stall(&read);

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
        self.waiting = later;
        self.write(now);
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
                Err(e) => panic!("logd: SYS_LOG_READ refused a {BATCH}-record buffer ({e:?})"),
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
                "logd: {} kernel record(s) were overwritten in a shard before this reader got \
                 to them",
                self.tail.lost() - self.lost
            );
            self.lost = self.tail.lost();
        }
        out
    }

    /// The lines, in stamp order, into the file, the readers and the console.
    fn write(&mut self, mut lines: Vec<Line>) {
        if lines.is_empty() {
            return;
        }
        lines.sort_by_key(|line| line.at_ns);
        let mut file = String::new();
        for line in &lines {
            match &line.kind {
                Kind::Kernel(record) => {
                    self.alert_unsynced |= record.severity() >= Some(Severity::Alert);
                    file.push_str(&format!("{}\n", record.tagged(&stamp(self.boot_local, record.at_ns))));
                }
                Kind::Program { tag, owner, said } => {
                    let severity = said.severity;
                    self.alert_unsynced |= severity >= Severity::Alert;
                    let tag = Tag::new(tag).expect("an origin's name is a tag");
                    let pid = (said.pid != *owner).then_some(said.pid);
                    let at = stamp(self.boot_local, said.at_ns);
                    let mut line = ProgramLine {
                        stamp: &at,
                        at_ns: said.at_ns,
                        severity,
                        tid: said.tid,
                        pid,
                        tag,
                        text: &said.text,
                    };
                    file.push_str(&format!("{line}\n"));
                    line.stamp = "";
                    let console = format!("{line}\n");
                    self.console_line(console.as_bytes());
                }
            }
        }
        // The file first: a reader is served only what /log already holds,
        // whenever the machine stops between the two.
        self.to_volume(file.as_bytes());
        self.hub.append(file.as_bytes());
    }

    /// One rendered program line for the console, held whole until it takes it.
    fn console_line(&mut self, line: &[u8]) {
        if self.console_held.len() + line.len() > CONSOLE_BYTES {
            self.console_unshown += 1;
            return;
        }
        self.console_held.extend_from_slice(line);
    }

    /// As many held lines as the console takes now; the rest wait for its room.
    fn feed_console(&mut self) {
        while !self.console_held.is_empty() {
            match self.console.write_nonblock(&self.console_held) {
                Ok(0) | Err(SyscallError::WouldBlock) => break,
                Ok(n) => {
                    self.console_held.drain(..n);
                }
                Err(e) => panic!("logd: its console refused a write: {e:?}"),
            }
        }
        if self.console_unshown > 0 && self.console_held.len() < CONSOLE_BYTES / 2 {
            toyos::warn!(
                "logd: {} line(s) went unshown on the console: it is slower than the log, and \
                 {DIR} has them",
                std::mem::replace(&mut self.console_unshown, 0)
            );
        }
    }

    /// Write a round to the volume, and make it durable when it is owed.
    fn to_volume(&mut self, text: &[u8]) {
        let Some(v) = self.volume.as_mut() else { return };
        let began = Instant::now();
        let mut refused = v.write(text).err().map(|e| (Step::Append, e.kind(), e.to_string()));
        let full = v.full();
        if refused.is_none() {
            let since = *self.unsynced_since.get_or_insert(began);
            if self.alert_unsynced || since.elapsed() >= SYNC_INTERVAL || full {
                refused = self.sync().err();
            }
        }
        // A volume that answered, and took longer than a log is worth doing it.
        if refused.is_none() && began.elapsed() > LOG_WRITE_BUDGET {
            refused =
                Some((Step::TooSlow, std::io::ErrorKind::Other, format!("it took {:?}", began.elapsed())));
        }
        self.answered(began, refused);
    }

    /// Make the volume durable once its oldest unsynced line has waited
    /// [`SYNC_INTERVAL`], whether or not this round wrote anything.
    fn sync_if_due(&mut self) {
        if self.unsynced_since.is_some_and(|since| since.elapsed() >= SYNC_INTERVAL) {
            let began = Instant::now();
            let refused = self.sync().err();
            self.answered(began, refused);
        }
    }

    /// Make the volume durable now.
    fn sync(&mut self) -> Result<(), (Step, std::io::ErrorKind, String)> {
        let Some(v) = self.volume.as_mut() else { return Ok(()) };
        v.sync().map_err(|e| (Step::Flush, e.kind(), e.to_string()))?;
        self.unsynced_since = None;
        self.alert_unsynced = false;
        Ok(())
    }

    /// What a round's write came to: rotation after a clean one, and the
    /// give-up policy after a refused one.
    fn answered(&mut self, began: Instant, refused: Option<(Step, std::io::ErrorKind, String)>) {
        let Some(path) = self.volume.as_ref().map(Volume::path) else { return };
        let Some((step, kind, why)) = refused else {
            self.retrying_since = None;
            if self.degraded {
                self.degraded = false;
                say!("logd: {DIR} answers at pace again - {path}");
            }
            self.rotate_if_full();
            return;
        };
        // The run of consecutive retries, which is what `LOG_WRITE_BUDGET`
        // bounds, from when its first round began.
        let first = self.retrying_since.is_none();
        let since = *self.retrying_since.get_or_insert(began);
        match fate(step, kind, since.elapsed()) {
            // Stop feeding the volume, say so once, and keep running.
            Fate::GiveUp => {
                toyos::error!(
                    "logd: {DIR} has not answered ({}: {why}) - this boot's log is on the console \
                     only from {path}",
                    step.as_str()
                );
                self.volume = None;
            }
            // Nothing is durable, so the next round's flush covers these bytes
            // as well as its own; one line per run.
            Fate::Retry => {
                if first {
                    toyos::warn!(
                        "logd: {DIR} would not start ({}: {why}) - nothing was lost, so {path} is \
                         still this boot's log and the next round is a retry",
                        step.as_str()
                    );
                }
            }
            // Every call answered, slowly: the round is durable.
            Fate::Degraded => {
                self.retrying_since = None;
                self.unsynced_since = None;
                self.alert_unsynced = false;
                if !self.degraded {
                    self.degraded = true;
                    toyos::warn!(
                        "logd: {DIR} answers but slowly ({}: {why}) - degraded, nothing lost, \
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
        if let Err(e) = v.rotate(|line| say!("{line}")) {
            toyos::error!("logd: {DIR} would not take a continuation ({e}) - {}", v.path());
            self.volume = None;
        }
    }

    /// init asked for the log whole before the machine stops: every line is
    /// written by now, so the volume is made durable and cut to its length,
    /// and init is told.
    fn flushed(&mut self) {
        let refused = self.sync().err();
        if refused.is_none() {
            if let Some(v) = self.volume.as_mut() {
                if let Err(e) = v.finish() {
                    toyos::error!("logd: {} would not be cut to its length: {e}", v.path());
                }
            }
        }
        self.answered(Instant::now(), refused);
        self.feed_console();
        if let Err(e) = self.from_init.signal(FLUSHED) {
            panic!("logd: init could not be told the log is whole: {e:?}");
        }
    }

    /// End a `--stall` once any program has said its `--stall-until` line.
    fn release_stall(&mut self, read: &[Said]) {
        let Some(stall) = &self.stall else { return };
        if read.iter().any(|r| r.text == stall.until.as_bytes()) {
            let origin = stall.origin.clone();
            self.stall = None;
            let (waiting, slots) = self
                .origins
                .iter()
                .find(|o| o.tag == origin)
                .map_or((0, 0), |o| o.waiting());
            say!(
                "logd: reading {origin} again, as `--stall-until` asked, with {waiting} of its \
                 ring's {slots} records waiting"
            );
        }
    }
}

/// The origin this program leaves unread (`--stall=<name>`), and the exact
/// line, from any program, that ends that (`--stall-until=<line>`): a test's
/// actuator, armed by nothing but a boot config's `args`.
///
/// It is how a boot stages a `logd` that stops reading one program while the
/// rest of the log — the test's own lines among them — still flows: that
/// program's ring fills and its writes are refused, which is the state a
/// writer that must never wait on logging is tested in.
struct Stall {
    origin: String,
    until: String,
}

impl Stall {
    fn from_args() -> Option<Stall> {
        let arg = |key: &str| std::env::args().find_map(|a| a.strip_prefix(key).map(str::to_string));
        match (arg("--stall="), arg("--stall-until=")) {
            (Some(origin), Some(until)) => Some(Stall { origin, until }),
            (None, None) => None,
            (origin, until) => panic!(
                "logd: `--stall` and `--stall-until` are armed together or not at all, and this \
                 boot gave {origin:?} and {until:?}"
            ),
        }
    }
}

/// This boot's file stem, and the local epoch second the machine booted at.
///
/// `None` for the stem is a boot that cannot be placed in time, which takes an
/// `unknown-NN` name — and the two ways to get there are named separately,
/// because "this machine has no clock" and "this machine has a clock whose zone
/// two readings cannot separate" are different facts about the machine.
fn boot_stamp() -> (Option<String>, Option<u64>, String) {
    match wall::local_now() {
        wall::Wall::Local { secs, offset_secs } => {
            let civil = Civil::from_unix_secs(secs);
            let uptime_secs = toyos_abi::clock::nanos_since_boot() / 1_000_000_000;
            (
                Some(format!("{}", civil.stem())),
                Some(secs.saturating_sub(uptime_secs)),
                format!("{civil} at UTC{:+} recovered from two readings", offset_secs / 3_600),
            )
        }
        wall::Wall::Unknown => {
            (None, None, "undated: this machine will not say what time it is".into())
        }
        // Named rather than guessed. The two candidates are the same time of day
        // on different days, so a file named from either is a day wrong half the
        // time; `wall`'s module header is the argument.
        wall::Wall::Ambiguous { east, west } => (
            None,
            None,
            format!(
                "undated: the clock is UTC{:+} or UTC{:+} on these two readings and nothing \
                 separates them",
                east / 3_600,
                west / 3_600
            ),
        ),
    }
}

/// A line's wall-clock stamp: the local second the machine booted at, plus the
/// line's own monotonic offset — which the line carries too, so `/log` holds
/// both clocks.
pub(crate) fn stamp(boot_local: Option<u64>, at_ns: u64) -> String {
    match boot_local {
        Some(base) => format!("{}", Civil::from_unix_secs(base + at_ns / 1_000_000_000)),
        // An undated boot writes the space the stamp would have taken, so the
        // columns line up and nothing has to be re-parsed to notice that a
        // machine had no clock.
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
}
