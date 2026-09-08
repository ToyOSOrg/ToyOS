//! The second sink: every line the file gets, over a TCP connection netd opens,
//! the instant the file gets it.
//!
//! **The file is the sink of record and this is not.** Nothing here can delay a
//! write to `/log`, refuse one, or lose a record from one — the whole of what
//! `logd` offers the stream is a line it has already written, and the offer
//! cannot fail. What a boot buys with it is a log that arrives *while the
//! machine is booting*: on the bench's ThinkPad the alternative is a USB stick
//! and a reboot into another operating system.
//!
//! # Why a thread
//!
//! Opening a TCP connection is a request to netd and an answer from it, and the
//! answer waits on a DHCP lease, a SYN and a peer. Every one of those is time
//! `logd`'s own loop does not have: the loop's next act is a batch of records
//! going to a file, and a daemon that stops writing the log because a cable is
//! out has traded the sink of record for the best-effort one. So the connection
//! is opened, and every byte written, on a thread of its own; between it and
//! the loop stands one bounded queue.
//!
//! # What a peer that will not take the log costs, and who counts it
//!
//! [`toyos_logstream::Backlog`] is that queue. A peer that stops taking bytes
//! closes the TCP window, netd stops draining the pipe, the writer blocks, the
//! queue fills, and lines are refused — counted, and said out loud in one line
//! that goes into the file like any other record. A drop nobody counts is the
//! one failure this design must not have: the file whole, the stream short, and
//! nothing saying by how much.
//!
//! **How much has to stall before that happens is not this queue's size.**
//! Between it and the peer stand the pipe netd reads (2 MiB, `kernel/src/
//! pipe.rs`) and netd's own send buffer (64 KiB), and both fill before one line
//! is refused: measured on the dev host, a `log-storm` at `--smp 8` put 4,213
//! lines — 674 KiB — through a peer that was reading nothing, without reaching
//! this queue at all. So the queue is what stands between this program and a
//! peer that answers *nothing*; the buffers below it absorb one that is merely
//! behind.
//!
//! # What it says when it cannot stream at all
//!
//! Once, in the boot's own log, and then never again. A refusal from the peer
//! is final — netd's own `ERR_CONNECTION_REFUSED` means the peer will keep
//! refusing — so the attempt stops there. Everything else is this machine not
//! being ready yet (netd has not claimed the card, the lease has not arrived),
//! so it is retried until [`OPEN_BOUND`] and then said once.

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use toyos::net::{self, NetError, TcpConnection};
use toyos_logstream::Backlog;

/// What netd is given for one connect.
///
/// It bounds netd's own attempt, not this one: the retry below is what covers a
/// machine whose network is not up yet, and a long timeout here would only make
/// each retry coarser.
const CONNECT_TIMEOUT_MS: u32 = 2_000;

/// Between two attempts. Short enough that a stream is live within a round of
/// netd becoming ready, long enough that a boot with no network is not spending
/// a core on saying so.
const RETRY_EVERY: Duration = Duration::from_millis(250);

/// How long this machine has to become able to open the connection at all.
///
/// netd has to claim the function, bring the link up and finish DHCP before the
/// first SYN can go out, and none of that is bounded by anything `logd` knows.
/// Measured on the dev host's TCG guest, `netd: ready` lands 0.4 s into the
/// boot; this is two orders of magnitude above it, so what it actually catches
/// is a machine that will never have a network — and on that machine the cost
/// of being wrong is one line in the log, thirty seconds late.
const OPEN_BOUND: Duration = Duration::from_secs(30);

/// How often a run of drops may put a line in the log.
///
/// **A run of loss is one fact, however many records it covers.** A stream that
/// cannot open at all refuses every line for the whole boot, and a report per
/// round of `logd`'s loop would be the log talking about itself instead of about
/// the machine. Each line carries the boot's running total, so the last one is
/// the whole answer and the ones before it are when it was still growing.
const DROP_REPORT_EVERY: Duration = Duration::from_secs(1);

/// The stream, from `logd`'s side: somewhere to put a line, and somewhere to
/// read what the stream owes the log.
pub struct Stream {
    shared: Arc<Shared>,
    /// When a run of drops last put a line in the log.
    said_at: Mutex<Option<Instant>>,
}

struct Shared {
    backlog: Mutex<Backlog>,
    /// Woken by [`Stream::offer`]; waited on by the writer.
    ready: Condvar,
    /// What the stream could not do, waiting to be written into the log. Said
    /// once per episode and then taken.
    trouble: Mutex<Option<String>>,
}

impl Stream {
    /// The stream this boot was told to open, or `None` when it was told to
    /// open none.
    ///
    /// **A boot that asked for no stream says nothing**: there is no failure to
    /// report, and a line in every ordinary boot's log about a feature nobody
    /// asked for is noise. A boot that asked for one with an address that is not
    /// one says so, because that is a failure.
    pub fn start(said: Option<&str>) -> Option<Self> {
        let value = said?;
        let shared = Arc::new(Shared {
            backlog: Mutex::new(Backlog::new()),
            ready: Condvar::new(),
            trouble: Mutex::new(None),
        });
        match toyos_logstream::endpoint(value) {
            Ok((addr, port)) => {
                let theirs = Arc::clone(&shared);
                // Named, because a backtrace out of a blocked write should say
                // which of `logd`'s two jobs was blocked.
                std::thread::Builder::new()
                    .name("log-stream".into())
                    .spawn(move || run(&theirs, addr, port))
                    .expect("logd: the log stream's writer could not be started");
            }
            Err(why) => {
                *shared.trouble.lock().expect("a fresh mutex is not poisoned") = Some(format!(
                    "logd: {}{value} is not an address ({}) - this boot's log is on /log only",
                    toyos_logstream::PARAM,
                    why.as_str()
                ));
            }
        }
        Some(Self { shared, said_at: Mutex::new(None) })
    }

    /// Offer one line — already written to the file — to the stream.
    ///
    /// It cannot fail and it cannot wait: the queue either takes the line or
    /// counts it. The only lock it touches is one the writer never holds across
    /// a write.
    pub fn offer(&self, line: &str) {
        let mut backlog = self.shared.backlog.lock().expect("the log stream's queue is poisoned");
        backlog.admit(line);
        drop(backlog);
        self.shared.ready.notify_one();
    }

    /// What the stream owes this boot's log: a failure it has not reported, or
    /// the count of what a listener slower than this machine cost. Empty on an
    /// ordinary round.
    pub fn owed(&self) -> Vec<String> {
        let mut owed = Vec::new();
        if let Some(said) = self.shared.trouble.lock().expect("a mutex is poisoned").take() {
            owed.push(said);
        }
        let mut said_at = self.said_at.lock().expect("a mutex is poisoned");
        if said_at.is_none_or(|at| at.elapsed() >= DROP_REPORT_EVERY) {
            if let Some(said) =
                self.shared.backlog.lock().expect("the log stream's queue is poisoned").report()
            {
                *said_at = Some(Instant::now());
                owed.push(said);
            }
        }
        owed
    }
}

/// Open the connection and write the queue into it, for the life of the
/// process.
fn run(shared: &Shared, addr: [u8; 4], port: u16) {
    let conn = match open(shared, addr, port) {
        Some(conn) => conn,
        None => return,
    };
    loop {
        let batch = {
            let mut backlog = shared.backlog.lock().expect("the log stream's queue is poisoned");
            while backlog.is_empty() {
                // The lock is given up here and taken again with something in
                // the queue; `offer` never waits behind this thread.
                backlog =
                    shared.ready.wait(backlog).expect("the log stream's queue is poisoned");
            }
            backlog.drain()
        };
        for line in batch {
            // **Blocking, and on purpose.** A full pipe is netd holding a
            // closed TCP window, which is the listener asking this machine to
            // slow down; waiting here is what turns that into a bounded queue
            // and a counted drop instead of an unbounded one.
            if !write_all(&conn, line.as_bytes()) {
                say(shared, format!(
                    "logd: the log stream to {}.{}.{}.{}:{port} closed - this boot's log \
                     continues on /log only",
                    addr[0], addr[1], addr[2], addr[3]
                ));
                return;
            }
        }
    }
}

/// The connection, or `None` once this machine has been given long enough to
/// have one.
fn open(shared: &Shared, addr: [u8; 4], port: u16) -> Option<TcpConnection> {
    let began = Instant::now();
    let at = format!("{}.{}.{}.{}:{port}", addr[0], addr[1], addr[2], addr[3]);
    loop {
        match net::tcp_connect(addr, port, CONNECT_TIMEOUT_MS) {
            Ok(conn) => return Some(conn),
            // **The peer answered, and its answer is final.** A refused SYN is
            // a listener that is not there; retrying it for thirty seconds
            // would delay the one line that says so and change nothing.
            Err(NetError::ConnectionRefused) => {
                say(shared, format!(
                    "logd: nothing is listening at {at} for this boot's log stream - \
                     this boot's log is on /log only"
                ));
                return None;
            }
            // The manifest gave this program no `netd`, so there is no network
            // to wait for either.
            Err(NetError::NetdNotFound) => {
                say(shared, format!(
                    "logd: this machine has no netd to reach {at} through - this boot's log \
                     is on /log only"
                ));
                return None;
            }
            Err(e) => {
                if began.elapsed() >= OPEN_BOUND {
                    say(shared, format!(
                        "logd: {at} did not answer in {:?} ({e:?}) - this boot's log is on \
                         /log only",
                        OPEN_BOUND
                    ));
                    return None;
                }
                std::thread::sleep(RETRY_EVERY);
            }
        }
    }
}

/// One `write` is one `SYS_WRITE` and a pipe may take part of a line; a line
/// that arrived in halves would be two lines on the listener's side.
fn write_all(conn: &TcpConnection, mut bytes: &[u8]) -> bool {
    while !bytes.is_empty() {
        match conn.tx.write(bytes) {
            Ok(0) | Err(_) => return false,
            Ok(n) => bytes = &bytes[n..],
        }
    }
    true
}

/// Leave one line for the log, without overwriting one nobody has read yet.
fn say(shared: &Shared, line: String) {
    let mut trouble = shared.trouble.lock().expect("a mutex is poisoned");
    if trouble.is_none() {
        *trouble = Some(line);
    }
}
