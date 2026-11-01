//! Two client ends sent over each other cannot keep each other open: a handle
//! travels only inside a frame, every server reads every frame, and a frame's
//! handles its server does not take are closed at its next read. So once both
//! clients are gone, both connections end.

use toyos::ipc::{Connection, FrameRx, RxStep};
use toyos::poller::{Poller, READABLE};
use toyos::{namespace, port, AsHandle, OwnedHandle};
use toyos_abi::syscall;

/// A failure ceiling only: an end that never comes is the defect.
const CEILING_NANOS: u64 = 10_000_000_000;

fn main() {
    let (acceptor, connector) = port::create().expect("a port");
    let names = namespace::build().add("conn", &connector).finish().expect("a namespace");
    let pair = || {
        let client = names.open("conn").expect("connect");
        (client, acceptor.accept().expect("accept"))
    };

    let ((client_a, server_a), (client_b, server_b)) = (pair(), pair());
    client_b.signal_with(1, [dup(&client_a)]).expect("A's end over B");
    client_a.signal_with(1, [dup(&client_b)]).expect("B's end over A");
    drop((client_a, client_b));
    let mut ended = [false, false];
    let mut rx = [FrameRx::<0>::new(), FrameRx::<0>::new()];
    let servers = [&server_a, &server_b];
    while ended != [true, true] {
        let poller = Poller::new(2);
        for (i, s) in servers.iter().enumerate().filter(|(i, _)| !ended[*i]) {
            poller.watch(*s, READABLE, i as u64);
        }
        let mut woke = false;
        poller.wait(1, CEILING_NANOS, |_| woke = true);
        assert!(woke, "two ends sent over each other kept each other open");
        for i in 0..2 {
            loop {
                match rx[i].pump(servers[i]) {
                    // Not taken: the next pump closes it, as any server's does.
                    RxStep::Frame { handles: 1, .. } => {}
                    RxStep::Frame { .. } | RxStep::Malformed => panic!("a frame the test never sent"),
                    RxStep::Eof => {
                        ended[i] = true;
                        break;
                    }
                    RxStep::Idle => break,
                }
            }
        }
    }
    println!("ends_sent_over_each_other_both_end: ok");
}

/// A second handle to `conn`'s end, to send.
fn dup(conn: &Connection) -> OwnedHandle {
    // SAFETY: nothing relies on being the source's only holder.
    let raw = unsafe { syscall::dup(conn.as_handle().raw()) }.expect("a second handle to the end");
    // SAFETY: a fresh duplicate, answered for by nothing else.
    unsafe { OwnedHandle::from_raw(raw) }
}
