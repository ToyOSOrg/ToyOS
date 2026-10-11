use std::sync::Arc;
use std::thread;
use std::time::Duration;
use std::process::{Command, Stdio};

use toyos_abi::syscall;
use toyos::poller::{Poller, READABLE};
use toyos::{namespace, port};

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("burn") => child_burn(),
        Some("sched-info") => child_sched_info(),
        _ => run_tests(),
    }
}

fn run_tests() {
    test_acceptor_isolation_io_uring();
    test_min_vruntime_invariant();
    test_connect_storm();
    println!("all sched_stress tests passed");
}

/// Burns until it is killed.
fn child_burn() -> ! {
    loop {
        std::hint::spin_loop();
    }
}

fn child_sched_info() {
    // Force a Runnable→NonRunnable→Runnable cycle so the lag we report is
    // the freshly-clamped post-wake value, not whatever drift has built up
    // since spawn. The kernel clamps lag to ±MAX_VRUNTIME_LAG_NS at the
    // transition; reading immediately after wake is the only point where
    // the invariant holds deterministically.
    thread::sleep(Duration::from_millis(10));
    let info = syscall::sched_info();
    println!("{} {} {}", info.vruntime, info.min_vruntime, info.lag);
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
        let handle = acc_a.into_raw();
        let poller = Poller::new(1);
        poller.watch_raw(handle, READABLE, 0);
        poller.wait(1, u64::MAX, |_| ());
        syscall::close(handle);
    });

    // Port b's, registered before the connection and asked only after a's poll
    // completed: the order, not a window, is what makes its silence a verdict.
    let b = acc_b.into_raw();
    let b_poller = Poller::new(1);
    b_poller.watch_raw(b, READABLE, 0);

    // Open a connection through port A's connector only.
    let ns = namespace::build().add("a", &con_a).finish().expect("a namespace naming port a");
    let client = ns.open("a").expect("open a");
    drop(client);

    a.join().expect("port a's watcher panicked");
    let mut b_poll_ready = false;
    b_poller.wait(0, 0, |_| b_poll_ready = true);
    syscall::close(b);

    assert!(!b_poll_ready, "port b's poll completed spuriously — acceptor isolation broken!");

    println!("  acceptor isolation (io_uring): ok");
}

/// Verify the post-wake lag invariant: when a process transitions from
/// non-runnable back to runnable, its lag is clamped to ±MAX_VRUNTIME_LAG_NS
/// (50ms). Read immediately after the wake transition.
fn test_min_vruntime_invariant() {
    let me = "/system/bin/test_rs_sched_stress";

    // Spawn 3 CPU burners to drive min_vruntime forward, until they are killed.
    let mut burners = Vec::new();
    for _ in 0..3 {
        burners.push(Command::new(me).arg("burn").spawn().expect("spawn burner"));
    }

    // A process that sleeps briefly (forces Runnable→NonRunnable→Runnable) and
    // reports lag immediately after wake, asked again until the burners have
    // moved min_vruntime off zero: a min_vruntime never updated is a hang the
    // harness ceiling reds.
    let (vruntime, min_vruntime, lag) = loop {
        let info_out = Command::new(me).arg("sched-info")
            .stdout(Stdio::piped()).spawn().expect("spawn sched-info")
            .wait_with_output().expect("wait sched-info");
        let output = String::from_utf8_lossy(&info_out.stdout);
        let parts: Vec<&str> = output.trim().split_whitespace().collect();
        assert_eq!(parts.len(), 3, "sched-info output should be 'vruntime min_vruntime lag', got: {output:?}");
        let vruntime: u64 = parts[0].parse().expect("parse vruntime");
        let min_vruntime: u64 = parts[1].parse().expect("parse min_vruntime");
        let lag: i64 = parts[2].parse().expect("parse lag");
        if min_vruntime > 0 {
            break (vruntime, min_vruntime, lag);
        }
    };

    for mut child in burners {
        child.kill().expect("kill a burner");
        child.wait().expect("reap a burner");
    }

    println!("  sched_info: vruntime={vruntime} min_vruntime={min_vruntime} lag={lag}");

    let max_lag_ns: i64 = 50_000_000;
    assert!(lag.abs() <= max_lag_ns,
        "post-wake lag ({lag}) exceeds ±MAX_VRUNTIME_LAG_NS ({max_lag_ns}) — \
         leave_runnable clamp not enforced or wake is not re-deriving from lag!");

    println!("  post-wake lag invariant: ok");
}

fn test_connect_storm() {
    let num_clients = 8;

    let (acceptor, connector) = port::create().expect("a port to storm");
    let ns = Arc::new(
        namespace::build().add("storm", &connector).finish().expect("a namespace naming it"),
    );

    let server = thread::spawn(move || {
        let handle = acceptor.into_raw();
        for _ in 0..num_clients {
            let conn = syscall::accept(handle).expect("accept failed");
            syscall::close(conn);
        }
        syscall::close(handle);
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
