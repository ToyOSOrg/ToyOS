//! Traffic fails over to the second of two links when the first goes down,
//! and returns when it comes back.
//!
//! On a machine with two virtio NICs, each on a QEMU user network of its own,
//! this job waits for both links' leases, then sends datagrams to an address
//! off both networks: they leave by the link netstack's default route takes,
//! which netstack's own count of each link's frames (`inspect net.link.<n>.sent`)
//! names. It asks the harness, on a stream over the other link, to set the
//! first link's NIC down; the next datagrams leave by the other link and none
//! by the first. Asked to set it up again, the datagrams return to it.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream, UdpSocket};
use std::time::{Duration, Instant};

use toyos_inspect::Value;

/// Off both user networks: RFC 5737's TEST-NET-1, which leaves by a default
/// route and answers nothing.
const FAR: (Ipv4Addr, u16) = (Ipv4Addr::new(192, 0, 2, 1), 9);

/// Datagrams a round sends: more than the frames a link carries of its own
/// in the time a round takes, so a round's link is the one past it.
const ROUND: u64 = 64;

/// A liveness ceiling on each wait, never a timing.
const BOUND: Duration = Duration::from_secs(90);

/// netstack's snapshot.
fn snapshot() -> BTreeMap<String, Value> {
    inspect::ask(toyos_inspect::NET).unwrap_or_else(|why| panic!("link_failover: netstack's snapshot: {why}"))
}

fn text<'a>(snap: &'a BTreeMap<String, Value>, key: &str) -> Option<&'a str> {
    match snap.get(key) {
        Some(Value::Text(text)) => Some(text),
        _ => None,
    }
}

/// The frames netstack has handed link `n`.
fn sent(snap: &BTreeMap<String, Value>, n: usize) -> u64 {
    match snap.get(&format!("net.link.{n}.sent")) {
        Some(Value::U64(sent)) => *sent,
        other => panic!("link_failover: link {n}'s sent count is {other:?}"),
    }
}

/// Polls netstack's snapshot until `done` holds of it.
fn until(what: &str, mut done: impl FnMut(&BTreeMap<String, Value>) -> bool) -> BTreeMap<String, Value> {
    let by = Instant::now() + BOUND;
    loop {
        let snap = snapshot();
        if done(&snap) {
            return snap;
        }
        assert!(Instant::now() < by, "link_failover: {what} within {BOUND:?}; netstack says {snap:?}");
        std::thread::yield_now();
    }
}

/// A round of datagrams, and each link's frames from before it to once one
/// link has carried the round.
fn round(socket: &UdpSocket) -> [u64; 2] {
    let before = snapshot();
    let before = [sent(&before, 0), sent(&before, 1)];
    for n in 0..ROUND {
        socket.send_to(&n.to_le_bytes(), FAR).expect("link_failover: a datagram queued");
    }
    let after = until("a link carrying the round", |snap| (0..2).any(|n| sent(snap, n) >= before[n] + ROUND));
    [sent(&after, 0) - before[0], sent(&after, 1) - before[1]]
}

/// The link carrying a round, and the other.
fn carried(by: [u64; 2]) -> (usize, usize) {
    if by[0] >= ROUND { (0, 1) } else { (1, 0) }
}

fn main() {
    let port: u16 = std::env::args()
        .nth(1)
        .and_then(|port| port.parse().ok())
        .expect("link_failover: the harness's port is this job's argument");
    let snap = until("both links' leases", |snap| {
        snap.get("net.links") == Some(&Value::U64(2)) && (0..2).all(|n| snap.get(&format!("net.link.{n}.lease.held")) == Some(&Value::Bool(true)))
    });
    // Each link's network, by its lease's address: QEMU's host is `.2` of it.
    let network = |n: usize| {
        let address = text(&snap, &format!("net.link.{n}.lease.address")).unwrap_or_else(|| panic!("link_failover: link {n} holds no lease"));
        let address: Ipv4Addr = address.split('/').next().and_then(|a| a.parse().ok()).expect("link_failover: an address");
        let [a, b, c, _] = address.octets();
        Ipv4Addr::new(a, b, c, 0)
    };
    let networks = [network(0), network(1)];

    let socket = UdpSocket::bind("0.0.0.0:0").expect("link_failover: a datagram socket");
    let (first, second) = carried(round(&socket));
    println!("  PASS: a round left by link {first}, on {}", networks[first]);

    // The harness, on a stream over the second link: its network's own host.
    let [a, b, c, _] = networks[second].octets();
    let mut control = TcpStream::connect(SocketAddr::from((Ipv4Addr::new(a, b, c, 2), port))).expect("link_failover: a stream to the harness over the second link");
    control.set_read_timeout(Some(BOUND)).expect("link_failover: a read timeout");
    let mut answers = BufReader::new(control.try_clone().expect("link_failover: the stream's reader"));
    let mut ask = |word: &str| {
        writeln!(control, "{word} {}", networks[first]).expect("link_failover: the ask");
        let mut line = String::new();
        answers.read_line(&mut line).expect("link_failover: the harness's answer");
        assert_eq!(line, format!("{word}\n"), "link_failover: the harness answered {line:?}");
    };

    ask("down");
    until("the first link down", |snap| text(snap, &format!("net.link.{first}.state")) == Some("down"));
    let by = round(&socket);
    assert_eq!(carried(by).0, second, "link_failover: the round left by {by:?}");
    assert_eq!(by[first], 0, "link_failover: nothing leaves by a link that is down");
    println!("  PASS: with link {first} down, a round left by link {second} and none by link {first}");

    ask("up");
    until("the first link up", |snap| text(snap, &format!("net.link.{first}.state")) == Some("up"));
    // Its lease verified again, the default route is the first link's.
    let by = Instant::now() + BOUND;
    while carried(round(&socket)).0 != first {
        assert!(Instant::now() < by, "link_failover: no round left by link {first} within {BOUND:?} of its return");
    }
    println!("  PASS: with link {first} up again, a round left by it");
    println!("link_failover: traffic failed over to the second link and returned to the first");
}
