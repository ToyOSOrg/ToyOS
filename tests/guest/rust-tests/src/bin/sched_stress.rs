use std::sync::Arc;
use std::thread;
use std::time::Duration;

use toyos::poller::{Poller, READABLE};
use toyos::{namespace, port};

fn main() {
    test_acceptor_isolation_io_uring();
    test_connect_storm();
    println!("all sched_stress tests passed");
}

// Test 1: Acceptor isolation via io_uring POLL_IN

/// Two acceptors create io_uring POLL_IN watches on their handles. A connection
/// opened through port A's connector must complete A's poll and only A's — a
/// readiness signal keyed on "some acceptor" instead of on the object wakes
/// every waiter in the system, which froze the compositor.
fn test_acceptor_isolation_io_uring() {
    let (acc_a, con_a) = port::create().expect("port a");
    let (acc_b, _con_b) = port::create().expect("port b");

    // Port a's poll, waited on with no deadline: its connection is the event.
    let a = thread::spawn(move || {
        let poller = Poller::new(1);
        poller.watch(&acc_a, READABLE, 0);
        poller.wait(1, u64::MAX, |_| ());
    });

    // Port b's, registered before the connection and asked only after a's poll
    // completed: the order, not a window, is what makes its silence a verdict.
    let b_poller = Poller::new(1);
    b_poller.watch(&acc_b, READABLE, 0);

    // Open a connection through port A's connector only.
    let ns = namespace::build().add("a", &con_a).finish().expect("a namespace naming port a");
    let client = ns.open("a").expect("open a");
    drop(client);

    a.join().expect("port a's watcher panicked");
    let mut b_poll_ready = false;
    b_poller.wait(0, 0, |_| b_poll_ready = true);
    drop(acc_b);

    assert!(!b_poll_ready, "port b's poll completed spuriously — acceptor isolation broken!");

    println!("  acceptor isolation (io_uring): ok");
}

fn test_connect_storm() {
    let num_clients = 8;

    let (acceptor, connector) = port::create().expect("a port to storm");
    let ns = Arc::new(
        namespace::build().add("storm", &connector).finish().expect("a namespace naming it"),
    );

    let server = thread::spawn(move || {
        for _ in 0..num_clients {
            drop(acceptor.accept().expect("accept failed"));
        }
    });

    thread::sleep(Duration::from_millis(50));

    let mut clients = Vec::new();
    for _ in 0..num_clients {
        let ns = Arc::clone(&ns);
        clients.push(thread::spawn(move || {
            drop(ns.open("storm").expect("open failed"));
        }));
    }

    for c in clients {
        c.join().unwrap();
    }
    server.join().unwrap();
    println!("  connect storm ({num_clients} clients): ok");
}
