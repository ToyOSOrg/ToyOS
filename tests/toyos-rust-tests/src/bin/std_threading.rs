use std::thread;
use std::time::Duration;

/// How long the child stays up, so the parent's first `try_wait` asks about a
/// process that is still running rather than about a race it lost.
const LINGER: Duration = Duration::from_millis(200);
/// The poll after it, and its ceiling — 25x `LINGER`, a liveness margin and not
/// a threshold.
const POLL: Duration = Duration::from_millis(10);
const POLLS: u32 = 500;

fn main() {
    // The `try_wait` target: alive long enough to be seen running, then gone
    // with a status to report.
    if std::env::args().nth(1).as_deref() == Some("linger") {
        thread::sleep(LINGER);
        return;
    }

    // Test available_parallelism returns > 0
    let n = thread::available_parallelism().expect("available_parallelism failed");
    assert!(n.get() > 0, "expected parallelism > 0, got {}", n.get());
    println!("available_parallelism = {}", n.get());

    // Test spawning threads that compute partial sums
    let handles: Vec<_> = (0..4)
        .map(|i| {
            thread::spawn(move || {
                let start = i * 250;
                let end = start + 250;
                (start..end).sum::<u64>()
            })
        })
        .collect();

    let total: u64 = handles.into_iter().map(|h| h.join().unwrap()).sum();
    let expected: u64 = (0..1000).sum();
    assert_eq!(total, expected, "partial sums mismatch: {total} != {expected}");

    // `try_wait` answers about the child and not about the wait: `None` while
    // it runs, `Some` once it has exited. Both halves are asserted, because a
    // `try_wait` stuck on either answer satisfies the other.
    let exe = std::env::current_exe().expect("current_exe failed");
    let mut child = std::process::Command::new(&exe)
        .arg("linger")
        .spawn()
        .expect("spawn child failed");

    if let Some(early) = child.try_wait().expect("try_wait failed") {
        panic!("try_wait reported {early} for a child that is still sleeping");
    }

    let mut exited = None;
    for _ in 0..POLLS {
        if let Some(status) = child.try_wait().expect("try_wait failed") {
            exited = Some(status);
            break;
        }
        thread::sleep(POLL);
    }
    let status = exited.unwrap_or_else(|| {
        panic!("try_wait never reported the exited child within {:?}", POLL * POLLS)
    });
    assert!(status.success(), "child exited with {status}");

    println!("all threading tests passed");
}
