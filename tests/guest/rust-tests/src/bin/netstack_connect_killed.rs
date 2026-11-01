//! A client killed while its connect is unanswered gives its place back to
//! the real netstack at once, not when the connect gives up.
//!
//! Until netstack answers a connect it holds the waker of the bell it later
//! watches the holder on, so that bell cannot report the holder leaving; the
//! holder's connection is what does. Two children connect to an address on
//! QEMU's user network that nothing answers, and each connect stays pending
//! until netstack gives the address up, within seconds. One is killed: its
//! place, read through netstack's `inspect`, comes back while the other still
//! runs and still holds its own, so the place came back for the kill and not
//! for the connect ending, which would have ended both. The survivor is
//! killed last and gives its place back too.
//!
//! The child connects only once netstack holds its lease: before it, a
//! connect is refused at once and holds no place. netstack announces neither
//! its lease nor a change of its count, so the parent asks it again until the
//! answer it waits for or the ceiling.

use std::collections::BTreeMap;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use toyos::endow;
use toyos_inspect::{Value, MSG_INSPECT, MSG_SNAPSHOT, NET};

const SELF_PATH: &str = "/system/bin/test_rs_netstack_connect_killed";

/// On QEMU's user network, where no host answers for it.
const NOWHERE: &str = "10.0.2.77:80";

/// A hang ceiling under QEMU, nothing more. The kill has to land before
/// netstack gives the address up (`nud::BROADCAST_SOLICIT` requests
/// `nud::RETRANS` apart, about 3 s), and a run that misses it is red at "a
/// child ended before it was killed".
const CEILING: Duration = Duration::from_secs(20);

/// Between two questions to netstack.
const ASK_AGAIN: Duration = Duration::from_millis(20);

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("connect") => {
            let got = std::net::TcpStream::connect(NOWHERE);
            panic!("a connect to {NOWHERE} answered {got:?}");
        }
        Some(other) => panic!("unknown role {other:?}"),
        None => test(),
    }
}

fn test() {
    until(
        |snapshot| snapshot.iter().any(|(key, value)| key.starts_with("net.link.") && key.ends_with(".lease.held") && *value == Value::Bool(true)),
        "netstack held a lease on a link",
    );
    let before = held(&snapshot());
    let connect = || Command::new(SELF_PATH).arg("connect").stdin(Stdio::null()).spawn().expect("a connecting child");
    let (mut killed, mut survivor) = (connect(), connect());
    until(|snapshot| held(snapshot) == before + 2, "both children's connects held a place");
    kill(&mut killed);
    until(|snapshot| held(snapshot) == before + 1, "the killed child's place came back");
    assert!(survivor.try_wait().expect("the survivor's state").is_none(), "the survivor's connect ended: something answered");
    assert_eq!(held(&snapshot()), before + 1, "the survivor's place went with the other's");
    kill(&mut survivor);
    until(|snapshot| held(snapshot) == before, "the survivor's place came back");
    println!("netstack_connect_killed: a client killed mid-connect gave its place back");
}

fn kill(child: &mut std::process::Child) {
    assert!(child.try_wait().expect("the child's state").is_none(), "a child ended before it was killed");
    child.kill().expect("kill a connecting child");
    let status = child.wait().expect("a killed child");
    assert!(!status.success(), "a killed child ended {status:?}");
}

/// Ask netstack until `wanted` holds of its snapshot, or panic at the
/// ceiling saying `what` never happened.
fn until(wanted: impl Fn(&BTreeMap<String, Value>) -> bool, what: &str) {
    let deadline = Instant::now() + CEILING;
    loop {
        let now = snapshot();
        if wanted(&now) {
            return;
        }
        assert!(Instant::now() < deadline, "{what} not within {CEILING:?}: netstack holds {:?} place(s)", now.get("net.places.held"));
        std::thread::sleep(ASK_AGAIN);
    }
}

/// netstack's snapshot now.
fn snapshot() -> BTreeMap<String, Value> {
    let conn = endow::service(NET.port).expect("the image gives test-runner netstack");
    conn.signal(MSG_INSPECT).expect("the inspect request");
    let mut rx = toyos::ipc::FrameRx::<{ toyos::ipc::MAX_FRAME_LEN as usize }>::new();
    let frame = rx.recv(&conn).expect("netstack's answer");
    assert_eq!(frame.msg_type, MSG_SNAPSHOT, "netstack answered something that is not a snapshot");
    toyos_inspect::decode(rx.payload(frame.payload_len), NET).expect("a snapshot for `net`")
}

/// The places a snapshot says netstack holds.
fn held(snapshot: &BTreeMap<String, Value>) -> u64 {
    match snapshot.get("net.places.held") {
        Some(Value::U64(held)) => *held,
        other => panic!("netstack's snapshot holds places.held as {other:?}"),
    }
}
