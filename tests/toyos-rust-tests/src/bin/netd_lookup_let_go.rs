//! A lookup whose client has left is let go at once, by netd's own loop, on a
//! network whose resolver never answers.
//!
//! The harness holds every frame this machine sends once it has its lease, so
//! no query reaches the one resolver the lease names. A lookup started here
//! holds its slot until its whole schedule has run out, [`toyos_dns::ROUNDS`]
//! waits of [`toyos_dns::WAIT_MS`], unless netd lets it go.
//!
//! **Hung up.** [`CAP`] lookups are started and held, and one more is refused
//! as exhausted, which is what shows all of them in flight. All of them hang
//! up, and the next lookup is not refused: it is asked, and ends timed out
//! when its schedule does, on netd's own wakes.
//!
//! **Spoke again.** A connection carries one request, so a client that says
//! more while its lookup is in flight is dropped: its connection closes with
//! no answer, long before the schedule would have answered it.
//!
//! `netd_lookup_let_go: ok` is the only success line.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use toyos::net::{dns_lookup, MsgType, NetError, NetdConn, PendingResponse};
use toyos::poller::{Poller, READABLE};
use toyos::Connection;
use toyos_abi::syscall::SyscallError;

/// netd's `resolve::MAX_LOOKUPS`, one declaration in `toyos_dns`.
const CAP: usize = toyos_dns::MAX_LOOKUPS;

/// Any name: nothing on this network answers one.
const NAME: &str = "unanswered.example";

/// How long a lookup's whole schedule runs with the one resolver the lease
/// names.
const SCHEDULE: Duration = Duration::from_millis(toyos_dns::ROUNDS as u64 * toyos_dns::WAIT_MS);

/// How long anything here may take before this program says so by name. A
/// bound, not a pace.
const WITHIN: Duration = Duration::from_secs(60);

fn main() {
    hung_up();
    spoke_again();
    println!("netd_lookup_let_go: ok");
}

fn hung_up() {
    let held: Vec<PendingResponse> = (0..CAP).map(|_| ask()).collect();
    assert_eq!(
        lookup_within(),
        Err(NetError::ResourceExhausted),
        "lookup {} was not refused as exhausted, so the {CAP} before it are not all in flight",
        CAP + 1
    );
    println!("netd_lookup_let_go: {CAP} lookups in flight, and the next refused");
    drop(held);
    let asked = Instant::now();
    let answer = lookup_within();
    let took = asked.elapsed();
    assert_ne!(
        answer,
        Err(NetError::ResourceExhausted),
        "{CAP} lookups hung up and the next was refused as exhausted: netd did not let them go"
    );
    assert_eq!(answer, Err(NetError::TimedOut), "a lookup nothing answers");
    assert!(took >= SCHEDULE, "a lookup nothing answers ended timed out after {took:?}, before its {SCHEDULE:?}");
    println!("netd_lookup_let_go: {CAP} hung up, and the next was asked and timed out after {took:?}");
}

fn spoke_again() {
    let chatty = toyos::endow::service("netd").expect("a connection to netd");
    chatty.send_bytes(MsgType::DnsLookup as u32, NAME.as_bytes()).expect("netd takes a lookup");
    let held: Vec<PendingResponse> = (1..CAP).map(|_| ask()).collect();
    assert_eq!(
        lookup_within(),
        Err(NetError::ResourceExhausted),
        "lookup {} was not refused as exhausted, so the {CAP} before it are not all in flight",
        CAP + 1
    );
    let spoke = Instant::now();
    chatty.send_bytes(MsgType::DnsLookup as u32, NAME.as_bytes()).expect("netd's end is still open");
    let answered = closed_or_answered(&chatty);
    let took = spoke.elapsed();
    assert_eq!(answered, 0, "a client that spoke again while its lookup ran was answered, after {took:?}");
    assert!(took < SCHEDULE, "a client that spoke again was dropped after {took:?}, not at once");
    println!("netd_lookup_let_go: a client that spoke again was dropped after {took:?}, unanswered");
    drop(held);
}

/// A lookup of [`NAME`], asked and left waiting.
fn ask() -> PendingResponse {
    NetdConn::connect()
        .expect("a connection to netd")
        .request_bytes(MsgType::DnsLookup, NAME.as_bytes())
        .expect("netd takes a lookup")
}

/// A lookup of [`NAME`] to its end, which has to come within [`WITHIN`].
fn lookup_within() -> Result<usize, NetError> {
    let (answered, answer) = mpsc::channel();
    std::thread::spawn(move || answered.send(dns_lookup(NAME, &mut [[0; 4]; 4])));
    answer.recv_timeout(WITHIN).unwrap_or_else(|_| panic!("netd did not end a lookup within {WITHIN:?}"))
}

/// Bytes netd wrote on `conn` before it closed, once it has closed, within
/// [`WITHIN`].
fn closed_or_answered(conn: &Connection) -> usize {
    let poller = Poller::new(1);
    let deadline = Instant::now() + WITHIN;
    let mut got = 0;
    let mut buf = [0u8; 64];
    loop {
        match conn.read_nonblock(&mut buf) {
            Ok(0) => return got,
            Ok(n) => got += n,
            Err(SyscallError::WouldBlock) => {
                let left = deadline.saturating_duration_since(Instant::now());
                assert!(!left.is_zero(), "netd neither answered nor closed a connection within {WITHIN:?}");
                poller.watch(conn, READABLE, 0);
                poller.wait(1, left.as_nanos() as u64, |_| {});
            }
            Err(e) => panic!("reading netd's end of a lookup's connection: {e:?}"),
        }
    }
}
