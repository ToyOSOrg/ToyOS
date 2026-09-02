use std::thread;
use std::time::Duration;

/// The child's life on the first attempt; a lost race quadruples it. **A host
/// fact, not a bound**: winning it means being scheduled between the spawn and
/// the question, which a twelve-wide TCG suite can take away.
const LINGER: Duration = Duration::from_millis(200);
/// Attempts at the running answer. Three covers a 64x slower host.
const TRIES: u32 = 3;
/// The poll for the exited answer, and its ceiling — a liveness margin.
const POLL: Duration = Duration::from_millis(10);
const POLLS: u32 = 500;

fn main() {
    // The `try_wait` target, told how long to stay up.
    if let Some(ms) = std::env::args().nth(1).and_then(|a| a.strip_prefix("linger=").map(String::from)) {
        thread::sleep(Duration::from_millis(ms.parse().expect("linger= wants milliseconds")));
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

    // Both answers, because a `try_wait` stuck on either satisfies the other.
    // The running answer re-arms with a longer child rather than reding the
    // first time this process loses the race to be asked.
    let exe = std::env::current_exe().expect("current_exe failed");
    let mut running = None;
    for attempt in 0..TRIES {
        let mut child = std::process::Command::new(&exe)
            .arg(format!("linger={}", (LINGER * 4u32.pow(attempt)).as_millis()))
            .spawn()
            .expect("spawn child failed");
        match child.try_wait().expect("try_wait failed") {
            None => {
                running = Some(child);
                break;
            }
            Some(_) => child.wait().map(|_| ()).expect("reap the raced child"),
        }
    }
    let mut child = running.unwrap_or_else(|| {
        panic!(
            "try_wait reported an exited child on all {TRIES} attempts, the last a {:?} sleep",
            LINGER * 4u32.pow(TRIES - 1)
        )
    });

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
