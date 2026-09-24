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
//! What it cannot do at all it says in the boot's own log. Nothing short of
//! netd's absence ends the first attempt before [`OPEN_BOUND`]: a machine with
//! no lease yet is netd's `ERR_NOT_CONNECTED`, and a peer's refusal is said at
//! once and asked again, because a listener that is not up yet refuses too.
//!
//! **A stream that has opened once is asked for again whenever it ends, with
//! no bound**: the listener leaving and coming back and netd being swapped
//! under the connection are both ends of one, and the boot asked for a stream
//! for as long as it runs. Each end and each reopening is one line in the log.
//! A line whose write failed is written again, whole, first on the next
//! connection; what netd had taken and not yet sent when it went is lost to
//! the stream and nowhere else.

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

/// Between two attempts at a peer that refused: each one is a SYN on the wire
/// and a reset back, where a machine with no address sends nothing.
const REFUSED_RETRY_EVERY: Duration = Duration::from_secs(1);

/// How long this machine has to become able to open the connection at all.
///
/// netd has to claim the function, bring the link up and finish DHCP before the
/// first SYN can go out, and none of that is bounded by anything `logd` knows.
/// What this catches is a machine that will never have a network, and on that
/// machine the cost of being wrong is one line in the log, late. **Wider than
/// two of netd's ten-second DHCP retries after a slow PHY's link comes up**,
/// because a stream given up on a machine that then leases is the one boot
/// the stream exists for.
const OPEN_BOUND: Duration = Duration::from_secs(60);

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
    /// What the stream has to say about itself, waiting to be written into the
    /// log. Said once per episode and then taken.
    trouble: Mutex<Vec<String>>,
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
            trouble: Mutex::new(Vec::new()),
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
        owed.append(&mut self.shared.trouble.lock().expect("a mutex is poisoned"));
        if let Some(said) = report {
            *said_at = Some(Instant::now());
            owed.push(said);
        }
        owed
    }
}

/// Open the connection and write the queue into it, for the life of the
/// process.
///
/// **One end, one line.** A connection that ends before it carried a line is
/// the same episode as the end before it — a listener that accepts and closes
/// at once is asked again once a second, and the log says so once — so the
/// end is said for the first connection and for one that delivered, and the
/// reopening once a reopened connection has delivered.
fn run(shared: &Shared, addr: [u8; 4], port: u16) {
    let at = format!("{}.{}.{}.{}:{port}", addr[0], addr[1], addr[2], addr[3]);
    let Some(mut conn) = open(shared, &at, addr, port, Some(OPEN_BOUND)) else { return };
    // What a failed write left unsent, which the next connection takes first.
    let mut unsent: Vec<String> = Vec::new();
    let (mut first, mut delivered, mut reopened) = (true, false, false);
    loop {
        let mut batch = std::mem::take(&mut unsent);
        {
            let mut backlog = shared.backlog.lock().expect("the log stream's queue is poisoned");
            while batch.is_empty() && backlog.is_empty() {
                // The lock is given up here and taken again with something in
                // the queue; `round` never waits behind this thread.
                backlog =
                    shared.ready.wait(backlog).expect("the log stream's queue is poisoned");
            }
            batch.extend(backlog.drain());
        }
        let mut sent = 0;
        let mut ended = None;
        for line in &batch {
            // **Blocking, and on purpose.** A full pipe is netd holding a
            // closed TCP window, which is the listener asking this machine to
            // slow down; waiting here is what turns that into a bounded queue
            // and a counted drop instead of an unbounded one.
            if let Err(why) = write_all(&conn, line.as_bytes()) {
                ended = Some(why);
                break;
            }
            sent += 1;
        }
        if sent > 0 && reopened {
            say(shared, format!("logd: the log stream to {at} is open again"));
            reopened = false;
        }
        delivered |= sent > 0;
        let Some(why) = ended else { continue };
        unsent = batch.split_off(sent);
        if first || delivered {
            say(shared, format!(
                "logd: the log stream to {at} ended ({why}) - asking for it again; /log has \
                 every record"
            ));
        }
        drop(conn);
        (first, delivered, reopened) = (false, false, true);
        std::thread::sleep(REFUSED_RETRY_EVERY);
        conn = match open(shared, &at, addr, port, None) {
            Some(conn) => conn,
            None => return,
        };
    }
}

/// The connection, or `None` once this machine has been given `bound` to have
/// one, or has no netd at all. `None` for `bound` asks until it opens.
fn open(
    shared: &Shared,
    at: &str,
    addr: [u8; 4],
    port: u16,
    bound: Option<Duration>,
) -> Option<TcpConnection> {
    let began = Instant::now();
    let spent = || bound.is_some_and(|bound| began.elapsed() >= bound);
    let mut refused = 0u32;
    loop {
        match net::tcp_connect(addr, port, CONNECT_TIMEOUT_MS) {
            Ok(conn) => {
                if refused > 0 {
                    say(shared, format!(
                        "logd: the log stream to {at} opened after {refused} refusal(s)"
                    ));
                }
                return Some(conn);
            }
            // **Said at once, and asked again until the bound.** A refused SYN
            // is a listener that is not there yet as often as one that never
            // will be — a host starting its listener late is the same answer —
            // so the line goes into the log now and the attempt goes on, at a
            // pace a peer that keeps refusing is not flooded by.
            Err(NetError::ConnectionRefused) => {
                if refused == 0 {
                    let until = match bound {
                        Some(bound) => format!("until {bound:?} after logd started"),
                        None => format!("every {REFUSED_RETRY_EVERY:?}"),
                    };
                    say(shared, format!(
                        "logd: nothing is listening at {at} for this boot's log stream - asking \
                         again {until}; /log has every record"
                    ));
                }
                refused += 1;
                if spent() {
                    return None;
                }
                std::thread::sleep(REFUSED_RETRY_EVERY);
            }
            // The manifest gave this program no `netd`, or the one it named
            // has ended for good, so there is no network to wait for either.
            Err(NetError::NetdNotFound) => {
                say(shared, format!(
                    "logd: this machine has no netd to reach {at} through - this boot's log \
                     is on /log only"
                ));
                return None;
            }
            Err(e) => {
                if spent() {
                    say(shared, format!(
                        "logd: {at} did not answer in {:?} ({e:?}) - this boot's log is on \
                         /log only",
                        began.elapsed()
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

/// Leave a line for the log, which `logd`'s next round takes.
fn say(shared: &Shared, line: String) {
    shared.trouble.lock().expect("a mutex is poisoned").push(line);
}
