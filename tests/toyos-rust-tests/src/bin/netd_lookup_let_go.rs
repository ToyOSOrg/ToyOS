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
//! no answer, where the schedule would have answered it timed out.
//!
//! `netd_lookup_let_go: ok` is the only success line.

use toyos::net::{dns_lookup, MsgType, NetError, NetdConn, PendingResponse};
use toyos::poller::{Poller, READABLE};
use toyos::Connection;
use toyos_abi::syscall::SyscallError;

/// netd's `resolve::MAX_LOOKUPS`, one declaration in `toyos_dns`.
const CAP: usize = toyos_dns::MAX_LOOKUPS;

/// Any name: nothing on this network answers one.
const NAME: &str = "unanswered.example";

fn main() {
    hung_up();
    spoke_again();
    println!("netd_lookup_let_go: ok");
}

fn hung_up() {
    let held: Vec<PendingResponse> = (0..CAP).map(|_| ask()).collect();
    assert_eq!(
        lookup(),
        Err(NetError::ResourceExhausted),
        "lookup {} was not refused as exhausted, so the {CAP} before it are not all in flight",
        CAP + 1
    );
    println!("netd_lookup_let_go: {CAP} lookups in flight, and the next refused");
    drop(held);
    let answer = lookup();
    assert_ne!(
        answer,
        Err(NetError::ResourceExhausted),
        "{CAP} lookups hung up and the next was refused as exhausted: netd did not let them go"
    );
    assert_eq!(answer, Err(NetError::TimedOut), "a lookup nothing answers");
    println!("netd_lookup_let_go: {CAP} hung up, and the next was asked and timed out");
}

fn spoke_again() {
    let chatty = toyos::endow::service("netd").expect("a connection to netd");
    chatty.send_bytes(MsgType::DnsLookup as u32, NAME.as_bytes()).expect("netd takes a lookup");
    let held: Vec<PendingResponse> = (1..CAP).map(|_| ask()).collect();
    assert_eq!(
        lookup(),
        Err(NetError::ResourceExhausted),
        "lookup {} was not refused as exhausted, so the {CAP} before it are not all in flight",
        CAP + 1
    );
    chatty.send_bytes(MsgType::DnsLookup as u32, NAME.as_bytes()).expect("netd's end is still open");
    let answered = closed_or_answered(&chatty);
    assert_eq!(answered, 0, "a client that spoke again while its lookup ran was answered");
    println!("netd_lookup_let_go: a client that spoke again was dropped, unanswered");
    drop(held);
}

/// A lookup of [`NAME`], asked and left waiting.
fn ask() -> PendingResponse {
    NetdConn::connect()
        .expect("a connection to netd")
        .request_bytes(MsgType::DnsLookup, NAME.as_bytes())
        .expect("netd takes a lookup")
}

/// A lookup of [`NAME`] to its end, with no deadline: a lookup netd never ends
/// is a hang the harness ceiling reds.
fn lookup() -> Result<usize, NetError> {
    dns_lookup(NAME, &mut [[0; 4]; 4])
}

/// Bytes netd wrote on `conn` before it closed, once it has closed, with no
/// deadline.
fn closed_or_answered(conn: &Connection) -> usize {
    let poller = Poller::new(1);
    let mut got = 0;
    let mut buf = [0u8; 64];
    loop {
        match conn.read_nonblock(&mut buf) {
            Ok(0) => return got,
            Ok(n) => got += n,
            Err(SyscallError::WouldBlock) => {
                poller.watch(conn, READABLE, 0);
                poller.wait(1, u64::MAX, |_| {});
            }
            Err(e) => panic!("reading netd's end of a lookup's connection: {e:?}"),
        }
    }
}
