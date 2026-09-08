//! The second sink: every line the file gets, over a TCP connection netd opens,
//! the instant the file gets it.
//!
//! **The file is the sink of record and this is not.** Nothing here may delay a
//! write to `/log`, refuse one, or lose a record from one: the whole of what
//! `logd` offers the stream is a line it has already written, and the offer
//! cannot fail.
//!
//! The connection is opened, and every byte written, on a thread of its own,
//! because opening it waits on a DHCP lease, a SYN and a peer, and `logd`'s own
//! loop may wait on none of those. Between that thread and the loop stands one
//! bounded queue ([`toyos_logstream::Backlog`]): a peer that stops taking bytes
//! closes the TCP window, netd stops draining the pipe, the writer blocks, the
//! queue fills, and lines are refused — counted, and said out loud in one line
//! that goes into the file and is never offered back to the queue.
//!
//! What it cannot do at all it says once, in the boot's own log. A refusal from
//! the peer is final — netd's `ERR_CONNECTION_REFUSED` means the peer will keep
//! refusing — so the attempt stops there; everything else is this machine not
//! being ready yet, so it is retried until [`OPEN_BOUND`] and then said once.

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use toyos::net::{self, NetError, TcpConnection};
use toyos_logstream::{Backlog, Due};

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
/// What this catches is a machine that will never have a network, and on that
/// machine the cost of being wrong is one line in the log, late.
const OPEN_BOUND: Duration = Duration::from_secs(30);

/// How often a run of drops may put a line in the log.
///
/// **A run of loss is one fact, however many records it covers.** A stream that
/// cannot open at all refuses every line for the whole boot, and a report per
/// round of `logd`'s loop would be the log talking about itself instead of about
/// the machine. Each line carries the boot's running total, so the last one is
/// the whole answer.
const DROP_REPORT_EVERY: Duration = Duration::from_secs(1);

/// The stream, from `logd`'s side: somewhere to put the lines the file has
/// taken, and what the stream owes the log in return.
pub struct Stream {
    shared: Arc<Shared>,
    /// When a run of drops last put a line in the log.
    said_at: Mutex<Option<Instant>>,
}

struct Shared {
    backlog: Mutex<Backlog>,
    /// Woken by [`Stream::round`]; waited on by the writer.
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
                say(&shared, format!(
                    "logd: {}{value} is not an address ({}) - this boot's log is on /log only",
                    toyos_logstream::PARAM,
                    why.as_str()
                ));
            }
        }
        Some(Self { shared, said_at: Mutex::new(None) })
    }

    /// Offer the lines the file has just taken, and answer what the stream owes
    /// this boot's log: a failure it has not reported, or the count of what a
    /// peer slower than this machine cost.
    ///
    /// **The caller writes those lines to the file and does not offer them
    /// back.** A drop report handed to a queue that is refusing is refused too,
    /// which owes another report, for the life of the boot;
    /// `Backlog::round` is where that rule is kept.
    ///
    /// It cannot fail and it cannot wait: the queue either takes a line or
    /// counts it. The only lock it touches is one the writer never holds across
    /// a write.
    pub fn round<'a>(&self, wrote: impl IntoIterator<Item = &'a str>) -> Vec<String> {
        let mut said_at = self.said_at.lock().expect("a mutex is poisoned");
        let due = match *said_at {
            Some(at) if at.elapsed() < DROP_REPORT_EVERY => Due::NotYet,
            _ => Due::Now,
        };
        let report = {
            let mut backlog =
                self.shared.backlog.lock().expect("the log stream's queue is poisoned");
            backlog.round(wrote, due)
        };
        self.shared.ready.notify_one();

        let mut owed = Vec::new();
        if let Some(said) = self.shared.trouble.lock().expect("a mutex is poisoned").take() {
            owed.push(said);
        }
        if let Some(said) = report {
            *said_at = Some(Instant::now());
            owed.push(said);
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
                // the queue; `round` never waits behind this thread.
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
            if let Err(why) = write_all(&conn, line.as_bytes()) {
                say(shared, format!(
                    "logd: the log stream to {}.{}.{}.{}:{port} ended ({why}) - this boot's log \
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
///
/// The error is carried out rather than collapsed: a stream that stopped is one
/// line in the log, and the reason is the whole of what that line is worth.
fn write_all(conn: &TcpConnection, mut bytes: &[u8]) -> Result<(), String> {
    while !bytes.is_empty() {
        match conn.tx.write(bytes) {
            Ok(0) => return Err("netd took none of it".to_string()),
            Ok(n) => bytes = &bytes[n..],
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(())
}

/// Leave one line for the log. The writer says at most one thing in its life —
/// it either fails to open or fails to write, and returns either way — and a
/// stream that never started one says its refusal from `start`.
fn say(shared: &Shared, line: String) {
    *shared.trouble.lock().expect("a mutex is poisoned") = Some(line);
}
