//! `/system/bin/logd` — the machine's log, written to a file by a process that can be
//! killed without taking the kernel with it.
//!
//! What this program replaces is the kernel's own file sink,
//! `kernel/src/log_file.rs`: a kernel module that appended the log ring to a
//! FAT volume **from the idle loop**, which is why an idle CPU on this machine
//! could be found four spinlocks deep inside a USB transfer with a userland
//! `println!` behind it. The kernel keeps the record ring and the console;
//! every policy about where records go — what the files are called, how many
//! there are, what happens when the stick stops answering, and who may read
//! the log as it is written — is here.
//!
//! # What goes in the log
//!
//! Two writers, told apart by the head this program gives each line
//! (`toyos_logstream`'s header is the form):
//!
//! - **the kernel's records**, read off its ring with `SYS_LOG_READ`;
//! - **every program's output.** `/system/bin/init` gives each program it
//!   starts a pipe for its stdout and stderr and moves the read end here with
//!   the manifest's name for the program, on a connection only init holds
//!   ([`toyos_logstream::ORIGINS`]). A line is that program's because it came
//!   out of that pipe. This program's own lines go in under its own name
//!   (`say!`), and **none of them waits on anything**: the main loop's go
//!   straight into its next round, and a reader thread's into a bounded queue
//!   the loop takes each round, where one past the bound is counted and the
//!   count said.
//!
//! A full pipe is a writer that waits for this program to read it: a program's
//! line is slowed, never dropped.
//!
//! # Three sinks, and only one of them is the sink of record
//!
//! The file is. [`serve`]'s readers are the second: the same lines, in the same
//! order, to whoever asks — over TCP and on this machine — from the boot's
//! first line however late they ask. The console is the third: each program's
//! line as the program wrote it, which on a machine with a serial port is the
//! one console there is. The readers and the console are each fed off this
//! loop's thread, and neither can slow the file: a reader is a thread with an
//! offset into what the file already has, and the console is a thread with a
//! bounded queue ([`Echo`]).
//!
//! # Its whole authority
//!
//! One `SysCap` duplicate carrying `Rights::LOG | Rights::WAIT`, which its
//! manifest row asks for by the name `logread`; the origins acceptor init
//! endows it; and what its row adds — a `netd` connector to serve the network,
//! and the `log` acceptor, where this machine's readers ask for the log and
//! `inspect` asks where it is going ([`inspect`]'s module, which grants
//! nothing). With the first it may read
//! every record every CPU wrote and park on the readiness source when there is
//! nothing new. It claims no device, opens no compositor connection and can
//! name no process. Writing files is ambient — a known residual of the
//! capability endowment, and not this program's to close.
//!
//! # Durability, which is a contract and not a hope
//!
//! Every batch is written, `fsync`ed and only then published: `LogTail::
//! publish_durable` carries the `at_ns` of the newest record now **on the
//! device**, the kernel clamps it and keeps the maximum in `LOG_DURABLE_NS`,
//! and a panicking kernel waits on that word for its own report to land.
//! Publishing before the sync would make the word a lie in exactly the case it
//! exists for, so the order here is load-bearing: write, sync, publish, never
//! two of the three.
//!
//! `SYS_FSYNC` reaches the device's own cache flush — before it did, it stopped
//! at the page cache, and this program calling the result durable would have
//! been a claim of durability that was not one.
//!
//! **A flush that would block is not a flush that failed**:
//! `io::ErrorKind::WouldBlock` from `sync_all` is `kernel/src/block.rs`'s
//! `BlockError::BudgetExpired`, which means the kernel declined to *start* the
//! operation on the caller's own clock — nothing was issued, the device is
//! untouched, and the bytes are still in the file waiting for the next batch's
//! flush. `policy::fate` is the whole decision, and `policy`'s own header is
//! the argument.
//!
//! # The kernel's records are the kernel's to put on the console
//!
//! `klogd` writes them to the console at the commit, and a second copy from
//! here would double every line on the wire.

/// One line of this program's own, under `logd`'s name like any other
/// program's — from any thread, and before the volume is open. It never waits
/// ([`said`]).
macro_rules! say {
    ($($arg:tt)*) => {{
        $crate::said(format!($($arg)*));
    }};
}

mod inspect;
mod policy;
mod serve;
mod store;
mod wall;

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::ThreadId;
use std::time::Instant;

use toyos::endow::{self, Endowments, SYSCAP_LABEL};
use toyos::ipc::{self, Connection, RxStep};
use toyos::log::{LogTail, Record};
use toyos::poller::{Poller, READABLE};
use toyos::port::Acceptor;
use toyos::syscap::SysCap;
use toyos::wake::{self, Bell, Waker};
use toyos::Pipe;
use toyos_abi::syscall::SyscallError;
use toyos_logstream::{Ended, Lines, ProgramLine, Tag, CARRIER, LOGD, MAX_TAG, ORIGINS, REGISTER, SERVICE};
use toyos_wallclock::Civil;

use policy::{fate, Fate, Step, LOG_WRITE_BUDGET};
use store::{Volume, DIR, MAX_LOG_BYTES, MAX_LOG_FILES, ROTATE_FAST_BYTES};

/// Records asked of `SYS_LOG_READ` at once: above `MAX_LOG_SHARDS`, which the
/// call refuses below, and large enough that an ordinary boot's burst is a
/// handful of syscalls rather than one per line.
const BATCH: usize = 64;

/// The name init registers its own pipe under (`userland/init`'s `Log::open`):
/// the one origin whose word on a swap this program acts on.
const INIT: &str = "init";

/// The poll's tokens: the kernel's readiness, init's origins connection, the
/// bell this program's other threads ring, and each origin's pipe from
/// [`ORIGIN_BASE`] up.
const KERNEL_TOKEN: u64 = 0;
const ORIGINS_TOKEN: u64 = 1;
const BELL_TOKEN: u64 = 2;
const ORIGIN_BASE: u64 = 3;

/// Programs whose output this program reads at once: what one poller can
/// watch beside the other three sources. A swap registers the new program's
/// pipe while the old one's may still have a writer, so this is a bound
/// init's own actions reach, and one past it is refused by name.
const MAX_ORIGINS: usize = Poller::MAX_HANDLES as usize - ORIGIN_BASE as usize;

/// What one origin's pipe gives a round at most: one read. **A bound on the
/// round, not on the program** — what is left is read the next round, and a
/// writer that outruns this waits on its full pipe — so a program that never
/// stops writing cannot keep this loop from the kernel's records or the file.
const READ_BYTES: usize = 64 * 1024;

/// How much of this boot a reader who connects late can be handed: as much as
/// the volume keeps, so the stream is never the shorter of the two.
const REPLAY_BYTES: usize = MAX_LOG_FILES * MAX_LOG_BYTES as usize;

/// Lines this program's other threads may have said and the main loop not yet
/// taken. A bound, because how many there are is a LAN host's choice: each
/// connection it opens is a line or two.
const THEIRS_LINES: usize = 256;

/// This program's own lines, before a round takes them.
struct Own {
    /// The main loop's thread: its lines are its next round's, unbounded,
    /// since it is the thread that takes them.
    main: OnceLock<ThreadId>,
    mine: Mutex<Vec<(u64, String)>>,
    theirs: Mutex<Theirs>,
    /// Rung after a push onto `theirs`, so a parked main loop wakes for it.
    bell: OnceLock<Waker>,
}

static OWN: Own = Own {
    main: OnceLock::new(),
    mine: Mutex::new(Vec::new()),
    theirs: Mutex::new(Theirs::new()),
    bell: OnceLock::new(),
};

/// Every thread's lines but the main loop's, to [`THEIRS_LINES`], and how
/// many found it full.
struct Theirs {
    lines: VecDeque<(u64, String)>,
    unsaid: u64,
}

impl Theirs {
    const fn new() -> Self {
        Self { lines: VecDeque::new(), unsaid: 0 }
    }

    /// Whether `line` was queued: past the bound it is counted instead.
    fn push(&mut self, at_ns: u64, line: String) -> bool {
        if self.lines.len() >= THEIRS_LINES {
            self.unsaid += 1;
            return false;
        }
        self.lines.push_back((at_ns, line));
        true
    }

    /// Every line queued, and how many went unsaid, since the last take.
    fn take(&mut self) -> (VecDeque<(u64, String)>, u64) {
        (std::mem::take(&mut self.lines), std::mem::replace(&mut self.unsaid, 0))
    }
}

/// `say!`'s one step, stamped as it is said, and never a wait: a push, and
/// from any thread but the main loop's a nonblocking byte on the bell.
fn said(line: String) {
    let at_ns = toyos_abi::syscall::clock_nanos();
    if OWN.main.get() == Some(&std::thread::current().id()) {
        OWN.mine.lock().expect("logd: its own lines are poisoned").push((at_ns, line));
        return;
    }
    if !OWN.theirs.lock().expect("logd: its own lines are poisoned").push(at_ns, line) {
        return;
    }
    OWN.bell.get().expect("logd: a thread other than the main loop exists only after the bell").wake();
}

/// Every line of this program's own said since the last round, into `round`:
/// the bell emptied first, so a push after this look rings it again.
fn own_lines(bell: &Bell, boot_local: Option<u64>, round: &mut Round) {
    bell.take();
    let mut lines = std::mem::take(&mut *OWN.mine.lock().expect("logd: its own lines are poisoned"));
    let (queued, unsaid) = OWN.theirs.lock().expect("logd: its own lines are poisoned").take();
    lines.extend(queued);
    if unsaid > 0 {
        lines.push((
            toyos_abi::syscall::clock_nanos(),
            format!("logd: {unsaid} line(s) from its reader threads went unsaid: their queue was full"),
        ));
    }
    let from = round.console.len();
    for (at_ns, line) in lines {
        said_by(LOGD, at_ns, line.as_bytes(), Ended::Yes, boot_local, round);
    }
    let own = round.console.split_off(from);
    round.own_console.extend_from_slice(&own);
}

/// The console, fed off the main loop's thread: a serial port spins per byte
/// (`uart_write_bytes` waits on the transmitter, and a byte at 115200 baud is
/// ten bit times, 87 us), so a round's echo written from the loop would hold the file for
/// seconds while the kernel's shards overwrite. The queue is bounded; what
/// does not fit is counted and the count said on the console, and `/log` has
/// every byte of it.
struct Echo {
    held: Mutex<(Vec<u8>, u64)>,
    more: Condvar,
}

/// What the console may fall behind the log by.
const ECHO_BYTES: usize = 1 << 20;

impl Echo {
    fn start() -> Arc<Echo> {
        let echo = Arc::new(Echo { held: Mutex::new((Vec::new(), 0)), more: Condvar::new() });
        let theirs = Arc::clone(&echo);
        std::thread::Builder::new()
            .name("log-echo".into())
            .spawn(move || theirs.run())
            .expect("logd: the console's thread could not be started");
        echo
    }

    /// `programs`' bytes where they fit, and this program's own lines always:
    /// they are few, and they are what says the console fell behind.
    fn push(&self, programs: &[u8], own: &[u8]) {
        let mut held = self.held.lock().expect("logd: the console's queue is poisoned");
        held.0.extend_from_slice(own);
        let bytes = programs;
        if held.0.len() + bytes.len() > ECHO_BYTES {
            held.1 += bytes.len() as u64;
        } else {
            held.0.extend_from_slice(bytes);
        }
        self.more.notify_one();
    }

    fn run(&self) -> ! {
        use std::io::Write;
        loop {
            let (bytes, dropped) = {
                let mut held = self.held.lock().expect("logd: the console's queue is poisoned");
                while held.0.is_empty() && held.1 == 0 {
                    held = self.more.wait(held).expect("logd: the console's queue is poisoned");
                }
                (std::mem::take(&mut held.0), std::mem::replace(&mut held.1, 0))
            };
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(&bytes);
            if dropped > 0 {
                let _ = writeln!(
                    out,
                    "logd: {dropped} bytes of programs' output were not echoed here: this console \
                     is slower than the log, and /log has them"
                );
            }
        }
    }
}

/// One program's output, as it is read.
struct Origin {
    tag: String,
    pipe: Pipe,
    lines: Lines,
}

/// The lines read in one round, each with the time it goes in the log under.
struct Round {
    lines: Vec<(u64, String)>,
    /// Each program's line as it wrote it, for the console.
    console: Vec<u8>,
    /// This program's own lines, for the console: never dropped there.
    own_console: Vec<u8>,
    /// init's words on a swap of [`CARRIER`] this round, in order.
    carrier: Vec<serve::Carrier>,
}

fn main() {
    // First, so every line this program says has somewhere to go.
    if OWN.main.set(std::thread::current().id()).is_err() {
        unreachable!("logd's main loop starts once");
    }
    let (ring, bell) = wake::pair().expect("logd: no pipe for its own threads' bell");
    if OWN.bell.set(ring).is_err() {
        unreachable!("logd's bell is made once");
    }
    let mut origins: Vec<Origin> = Vec::new();

    // The two refusals below are said on the console: there is no log for them
    // to be in.
    let Some(cap) = Endowments::get().take::<SysCap>(SYSCAP_LABEL) else {
        eprintln!("logd: this program holds no system capability, so it holds no `logread`");
        std::process::exit(1);
    };
    let Some(acceptor) = Endowments::get().take::<Acceptor>(ORIGINS) else {
        eprintln!("logd: init endowed no `{ORIGINS}`, so no program's output can reach this log");
        std::process::exit(1);
    };
    // init connected before it started anything, so this is already queued.
    let from_init: Connection = acceptor.accept().expect("logd: init's origins connection");
    let mut from_init_rx = ipc::FrameRx::<MAX_TAG>::new();

    let rotate_at = if std::env::args().any(|a| a == "--rotate-fast") {
        ROTATE_FAST_BYTES
    } else {
        MAX_LOG_BYTES
    };
    let mut held = Held::from_args();

    // The wall clock, read once. The kernel reads the RTC once too, so a second
    // reading later in the boot would answer out of the same anchor and tell
    // this program nothing new.
    let (stem, boot_local, zone) = boot_stamp();

    let mut volume = Volume::open(stem, rotate_at, |line| say!("{line}"));
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
    let echo = Echo::start();

    let published = Arc::new(inspect::Published::new(hub.network()));
    if let Some(acceptor) = endow::acceptor(SERVICE) {
        inspect::serve(acceptor, Arc::clone(&published), Arc::clone(&hub));
    }

    let mut tail = LogTail::new();
    let mut buf = vec![Record::EMPTY; BATCH];
    // Programs' lines read and not yet written: each waits for the kernel's
    // records stamped before it (`read_origins`'s doc).
    let mut waiting: Vec<(u64, String)> = Vec::new();
    let poller = Poller::new(Poller::MAX_HANDLES);
    let mut lost = 0u64;
    // When the current run of consecutive retries began, or `None` when the
    // last batch was answered. `policy::fate` bounds the run and not the round.
    let mut retrying_since: Option<Instant> = None;
    // Whether the volume is currently degraded — answering, slower than
    // `LOG_WRITE_BUDGET` a round — so the state is announced once per episode
    // rather than once per slow batch.
    let mut degraded = false;
    loop {
        let state = match (&volume, retrying_since, degraded) {
            (None, _, _) => inspect::State::ConsoleOnly,
            (Some(_), Some(_), _) => inspect::State::Retrying,
            (Some(_), None, true) => inspect::State::Degraded,
            (Some(_), None, false) => inspect::State::Writing,
        };
        published.publish(volume.as_ref(), state, tail.lost());

        // **Armed before anything is read**, in the shape every reader of an
        // edge needs: what arrives after a read and before the park has a
        // registration waiting for it. `min_complete` 0 with no timeout
        // submits the entries and returns. A watch is one-shot, so one that
        // answered here is spent: its source is read below and no longer
        // armed, and this round may not park on it. A source this round will
        // not read is not armed either: it stays readable, and a watch on it
        // would never let this loop park.
        if held.ring.is_none() {
            poller.watch(&cap, READABLE, KERNEL_TOKEN);
        }
        poller.watch(&from_init, READABLE, ORIGINS_TOKEN);
        poller.watch(&bell, READABLE, BELL_TOKEN);
        for (i, origin) in origins.iter().enumerate() {
            if !held.holds(origin) {
                poller.watch(&origin.pipe, READABLE, ORIGIN_BASE + i as u64);
            }
        }
        let mut spent = false;
        poller.wait(0, 0, |_| spent = true);

        let joined = registered(&from_init, &mut from_init_rx, &mut origins);
        let mut round =
            Round { lines: Vec::new(), console: Vec::new(), own_console: Vec::new(), carrier: Vec::new() };
        if waiting.is_empty() {
            read_origins(&mut origins, boot_local, &mut round, &mut held);
            for word in round.carrier.drain(..) {
                hub.carrier(word);
            }
        }
        own_lines(&bell, boot_local, &mut round);
        waiting.append(&mut round.lines);

        // After the pipes, so a line read this round is stamped before every
        // record this read can hand back that was written after it.
        let batch: &[Record] = if held.ring.is_some() {
            &[]
        } else {
            match tail.read(&cap, &mut buf) {
                Ok(batch) => batch,
                Err(e) => {
                    // The one call this program is built around. A refusal is
                    // not survivable by retrying — the buffer and the rights
                    // are the same every time — so it says so and stops.
                    eprintln!("logd: SYS_LOG_READ refused a {BATCH}-record buffer ({e:?})");
                    std::process::exit(1);
                }
            }
        };
        if tail.lost() > lost {
            // One line per hole rather than one per read, and it goes in the
            // file it is a hole in: a later round carries it.
            say!(
                "logd: {} record(s) were overwritten in a shard before this reader got to them",
                tail.lost() - lost
            );
            lost = tail.lost();
        }

        // The newest *record* this round writes, and never a program's line: it
        // is what `publish_durable` promises the kernel is on the device, and a
        // program's later stamp would promise records this round never read.
        let newest = batch.last().map_or(0, |r| r.at_ns);
        // A short batch is a ring this reader has caught up with.
        let caught_up = batch.len() < BATCH;
        for record in batch {
            let line = format!("{}\n", record.tagged(&stamp(boot_local, record.at_ns)));
            round.lines.push((record.at_ns, line));
        }
        let due = if caught_up { u64::MAX } else { newest };
        let (now, later): (Vec<_>, Vec<_>) =
            waiting.drain(..).partition(|(at_ns, _)| *at_ns <= due);
        round.lines.extend(now);
        waiting = later;

        // Each program's line on the console as the program wrote it, off this
        // thread ([`Echo`]).
        if !round.console.is_empty() || !round.own_console.is_empty() {
            echo.push(&round.console, &round.own_console);
        }

        if round.lines.is_empty() {
            // **Nothing new, so park until something is.** `SYS_LOG_READ` and a
            // pipe read here never block by design; this is the other half.
            // Every source was armed before it was read, so what lands after
            // the reads is a completion this wait takes, and nothing else is
            // worth waking for: `klogd` posts after each drain, a pipe with
            // bytes in it or a writer gone is readable, and so is the bell
            // once another thread has said a line. A watch spent at the arm, a
            // pipe that joined this round, or a line this loop said itself, is
            // not armed, so the round goes again instead: a program's line
            // written in pieces lands its later pieces on exactly that source.
            let mine = !OWN.mine.lock().expect("logd: its own lines are poisoned").is_empty();
            if spent || joined || mine {
                continue;
            }
            poller.wait(1, u64::MAX, |_| {});
            continue;
        }
        // One order for the file and every reader: the time each line was
        // stamped at, the kernel's own merge kept among its records.
        round.lines.sort_by_key(|(at_ns, _)| *at_ns);
        let text: String = round.lines.into_iter().map(|(_, line)| line).collect();

        let Some(v) = volume.as_mut() else {
            hub.append(text.as_bytes());
            continue;
        };
        let began = Instant::now();
        let mut refused: Option<(Step, std::io::ErrorKind, String)> = None;
        if let Err(e) = v.write(text.as_bytes()) {
            refused = Some((Step::Append, e.kind(), e.to_string()));
        }
        // **After the file has them.** A reader is a thread with an offset into
        // what this hands it, and none of them can make this wait.
        hub.append(text.as_bytes());
        if refused.is_none() {
            if let Err(e) = v.sync() {
                refused = Some((Step::Flush, e.kind(), e.to_string()));
            }
        }
        // A volume that answered, and took longer than a log is worth doing it.
        // Checked after the write rather than before, because there is nothing
        // to cancel: `SYS_WRITE` and `SYS_FSYNC` do not come back until the
        // transport's own bound has expired, so the only place to notice is
        // here.
        if refused.is_none() && began.elapsed() > LOG_WRITE_BUDGET {
            refused = Some((
                Step::TooSlow,
                std::io::ErrorKind::Other,
                format!("it took {:?}", began.elapsed()),
            ));
        }

        match refused {
            None => {
                // **After the sync and never before it.** This word is what a
                // panicking kernel waits on, so publishing it for a record that
                // is only in the page cache would lose the report in exactly
                // the case the wait exists for.
                tail.publish_durable(newest);
                retrying_since = None;
                if degraded {
                    degraded = false;
                    say!("logd: {DIR} answers at pace again - {}", v.path());
                }
                if v.full() {
                    if let Err(e) = v.rotate(|line| say!("{line}")) {
                        say!("logd: {DIR} would not take a continuation ({e}) - {}", v.path());
                        volume = None;
                    }
                }
            }
            Some((step, kind, why)) => {
                // The run of consecutive retries, which is what
                // `LOG_WRITE_BUDGET` bounds. `began` and not `Instant::now()`:
                // the run starts when the first refused round started, so the
                // time this batch spent being refused is inside it.
                let first = retrying_since.is_none();
                let since = *retrying_since.get_or_insert(began);
                match fate(step, kind, since.elapsed()) {
                    // The give-up policy, in order: stop feeding the volume, say so once, and
                    // keep running. It does not exit and does not queue for a
                    // device that is not answering — "I stop waiting for this
                    // stick and say so" is the whole policy for a device fact.
                    Fate::GiveUp => {
                        say!(
                            "logd: {DIR} has not answered ({}: {why}) - this boot's log is on \
                             the console only from {}",
                            step.as_str(),
                            v.path()
                        );
                        volume = None;
                    }
                    // **Nothing is published**, because nothing is durable: the
                    // bytes are in the file and the next batch's flush covers
                    // them as well as its own, so the kernel's `LOG_DURABLE_NS`
                    // stays a word that is true.
                    //
                    // One line per *run* and not per round: a loaded host
                    // refuses several batches in a row, and a line each is the
                    // feedback loop `LOG_WRITE_BUDGET`'s own doc measures.
                    Fate::Retry => {
                        if first {
                            say!(
                                "logd: {DIR} would not start ({}: {why}) - nothing was lost, so \
                                 {} is still this boot's log and the next batch is a retry",
                                step.as_str(),
                                v.path()
                            );
                        }
                    }
                    // Every call answered, so the batch is durable and is
                    // published — this is the one refused-shaped outcome that
                    // is a success. The volume is kept whole: degraded is not
                    // dead, and slowness is the kernel's to bound now
                    // (`block::DEADMAN`), not this program's to punish.
                    Fate::Degraded => {
                        tail.publish_durable(newest);
                        retrying_since = None;
                        if !degraded {
                            degraded = true;
                            say!(
                                "logd: {DIR} answers but slowly ({}: {why}) - degraded, nothing \
                                 lost, {} is still this boot's log",
                                step.as_str(),
                                v.path()
                            );
                        }
                        if v.full() {
                            if let Err(e) = v.rotate(|line| say!("{line}")) {
                                say!(
                                    "logd: {DIR} would not take a continuation ({e}) - {}",
                                    v.path()
                                );
                                volume = None;
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Take every `REGISTER` init has sent: a program's name and the read end of
/// its output pipe. Whether any came: a pipe taken here was not armed before
/// this round's reads, so the caller may not park on it.
///
/// **init is the only peer this connection has**, so a frame that is not one,
/// or a name that is no tag, is init's bug and a loud end: init checks the
/// name before it sends. A pipe past [`MAX_ORIGINS`] is refused by name and
/// let go rather than ending this program: a swap registers its program's
/// pipe while the old one's may still have a writer, so it is reachable.
fn registered(conn: &Connection, rx: &mut ipc::FrameRx<MAX_TAG>, origins: &mut Vec<Origin>) -> bool {
    let mut came = false;
    loop {
        match rx.pump(conn) {
            RxStep::Idle => return came,
            RxStep::Eof => panic!("logd: init closed the origins connection"),
            RxStep::Malformed => panic!("logd: init sent a frame the origins protocol cannot carry"),
            RxStep::Frame { msg_type, payload_len } => {
                assert_eq!(msg_type, REGISTER, "logd: init sent frame type {msg_type} for an origin");
                let name = std::str::from_utf8(rx.payload(payload_len))
                    .ok()
                    .and_then(Tag::new)
                    .unwrap_or_else(|| panic!("logd: init registered an origin under no name"));
                let Some([raw]) = conn.recv_handles_exact::<1>() else {
                    panic!("logd: init registered {name:?} with no pipe");
                };
                // SAFETY: the kernel moved this handle into this table with the
                // frame that names it, and nothing else answers for it.
                let pipe = unsafe { Pipe::from_raw(raw) };
                if origins.len() >= MAX_ORIGINS {
                    say!(
                        "logd: refusing {}'s output: {MAX_ORIGINS} programs' pipes are already read",
                        name.as_str()
                    );
                    continue;
                }
                origins.push(Origin { tag: name.as_str().to_string(), pipe, lines: Lines::new() });
                came = true;
            }
        }
    }
}

/// One read of every origin's pipe, into `round`. An origin whose writers are
/// all gone says what it left unfinished and is dropped.
///
/// **A line is stamped when this program reads it, not when its program wrote
/// it**, and written after every kernel record stamped before that: the caller
/// holds it until the ring is caught up or its records have passed the stamp,
/// and reads no pipe while it holds any. So a line is never put ahead of a
/// record written before it was read — and a line written before a record and
/// read after it is put after that record, which is the order a program's last
/// words and its own `exit:` record can land in.
fn read_origins(origins: &mut Vec<Origin>, boot_local: Option<u64>, round: &mut Round, held: &mut Held) {
    let mut chunk = vec![0u8; READ_BYTES];
    let (mut released, mut ring_released) = (false, false);
    origins.retain_mut(|origin| {
        if held.holds(origin) {
            return true;
        }
        let at_ns = toyos_abi::syscall::clock_nanos();
        let tag = origin.tag.as_str();
        let mut said = |line: &[u8], ended| {
            released |= held.stall.as_ref().is_some_and(|s| s.until.as_bytes() == line);
            ring_released |= held.ring.as_ref().is_some_and(|until| until.as_bytes() == line);
            said_by(tag, at_ns, line, ended, boot_local, round)
        };
        match origin.pipe.read_nonblock(&mut chunk) {
            Ok(0) => {
                origin.lines.finish(&mut said);
                false
            }
            Ok(n) => {
                origin.lines.push(&chunk[..n], &mut said);
                true
            }
            Err(SyscallError::WouldBlock) => true,
            Err(e) => panic!("logd: {tag}'s pipe refused a read: {e:?}"),
        }
    });
    if ring_released {
        held.ring = None;
        say!("logd: reading the kernel's records again, as `--hold-ring-until` asked");
    }
    if released {
        let stall = held.stall.take().expect("released only while one is armed");
        // Everything the stall left waiting, in one read, so the line below
        // says how full the pipe was when it ended.
        let mut waiting = vec![0u8; STALL_READ_BYTES];
        let mut read = 0;
        if let Some(Origin { tag, pipe, lines }) = origins.iter_mut().find(|o| o.tag == stall.origin) {
            let at_ns = toyos_abi::syscall::clock_nanos();
            match pipe.read_nonblock(&mut waiting) {
                Ok(n) => {
                    read = n;
                    lines.push(&waiting[..n], |line, ended| {
                        said_by(tag, at_ns, line, ended, boot_local, round)
                    });
                }
                Err(SyscallError::WouldBlock) => {}
                Err(e) => panic!("logd: {tag}'s pipe refused a read: {e:?}"),
            }
        }
        say!(
            "logd: reading {} again, as `--stall-until` asked, with {read} bytes waiting",
            stall.origin
        );
    }
}

/// Larger than any pipe: the kernel's is one 2 MiB page.
const STALL_READ_BYTES: usize = 4 << 20;

/// A test's actuators, armed by nothing but a boot config's `args`.
struct Held {
    stall: Option<Stall>,
    /// `--hold-ring-until=<line>`: no record is read off the kernel's ring from
    /// the boot's start until a program says exactly `<line>`, and the round
    /// that reads it reads the ring after it. It is how a boot stages a
    /// program's line read while more than [`BATCH`] records written before it
    /// are unread, which is the state the hold on a program's line exists for.
    ring: Option<String>,
}

/// The origin this program leaves unread (`--stall=<name>`), and the exact
/// line, from any program, that ends that (`--stall-until=<line>`).
///
/// It is how a boot stages a `logd` that stops reading one program while the
/// rest of the log — the test's own lines among them — still flows: that
/// program's pipe fills and its next write waits, which is the state a writer
/// that must never wait on logging is tested in. Its end takes the whole pipe
/// in one read, past [`READ_BYTES`]'s bound on a round, so the line that says it
/// ended can say how full the pipe was.
struct Stall {
    origin: String,
    until: String,
}

impl Held {
    fn from_args() -> Held {
        let arg = |key: &str| std::env::args().find_map(|a| a.strip_prefix(key).map(str::to_string));
        let stall = match (arg("--stall="), arg("--stall-until=")) {
            (Some(origin), Some(until)) => Some(Stall { origin, until }),
            (None, None) => None,
            (origin, until) => panic!(
                "logd: `--stall` and `--stall-until` are armed together or not at all, and this \
                 boot gave {origin:?} and {until:?}"
            ),
        };
        Held { stall, ring: arg("--hold-ring-until=") }
    }

    fn holds(&self, origin: &Origin) -> bool {
        self.stall.as_ref().is_some_and(|s| s.origin == origin.tag)
    }
}

/// One program's line, into this round: its form in the log, a line of its
/// own, and its bytes for the console as the program wrote them — a line the
/// program had not ended is not ended there either.
fn said_by(
    tag: &str,
    at_ns: u64,
    line: &[u8],
    ended: Ended,
    boot_local: Option<u64>,
    round: &mut Round,
) {
    let tag = Tag::new(tag).expect("an origin's name was a tag when it registered");
    let stamp = stamp(boot_local, at_ns);
    round.lines.push((at_ns, format!("{}\n", ProgramLine { stamp: &stamp, at_ns, tag, text: line })));
    // Only init's own pipe can say it: the same words from any other program
    // are that program's, and would turn every reader away for good.
    if tag.as_str() == INIT {
        use toyos_swap::Word;
        match std::str::from_utf8(line).ok().and_then(|text| toyos_swap::heard(text, CARRIER)) {
            Some((Word::Accepted, _)) => round.carrier.push(serve::Carrier::Leaving),
            // Each said once the netd it replaces has been waited for.
            Some((Word::Started | Word::Failed | Word::Restored | Word::Gone, _)) => {
                round.carrier.push(serve::Carrier::Back)
            }
            Some((Word::Refused | Word::Stopping | Word::InService, _)) | None => {}
        }
    }
    round.console.extend_from_slice(line);
    if ended == Ended::Yes {
        round.console.push(b'\n');
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
            let uptime_secs = toyos_abi::syscall::clock_nanos() / 1_000_000_000;
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
    use super::*;

    /// The `log` port answers a reader and `inspect` on one acceptor, told
    /// apart by the request's frame type alone.
    #[test]
    fn readers_and_inspect_share_one_port_and_never_one_request() {
        assert_eq!(toyos_inspect::LOG.port, SERVICE);
        assert_ne!(toyos_logstream::READ, toyos_inspect::MSG_INSPECT);
    }

    /// **A LAN host cannot grow logd by connecting.** Each connection is a
    /// line or two from a reader thread; past [`THEIRS_LINES`] a line is
    /// counted and not kept, the first lines stay, and a take empties both.
    #[test]
    fn a_thread_line_past_the_bound_is_counted_and_not_kept() {
        let mut theirs = Theirs::new();
        let pushed: Vec<bool> = (0..THEIRS_LINES + 5).map(|i| theirs.push(i as u64, format!("line {i}"))).collect();
        assert!(pushed[..THEIRS_LINES].iter().all(|&kept| kept));
        assert!(pushed[THEIRS_LINES..].iter().all(|&kept| !kept));
        let (lines, unsaid) = theirs.take();
        assert_eq!((lines.len(), unsaid), (THEIRS_LINES, 5));
        assert_eq!(lines.back().map(|(_, l)| l.as_str()), Some(format!("line {}", THEIRS_LINES - 1).as_str()));
        let (lines, unsaid) = theirs.take();
        assert_eq!((lines.len(), unsaid), (0, 0), "a take empties the queue and the count");
    }
}
