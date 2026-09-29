use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

/// Between two asks of the exited answer. A pace and never a verdict: a child
/// `try_wait` never sees exit is a hang the harness ceiling reds.
const POLL: Duration = Duration::from_millis(10);

fn main() {
    // The `try_wait` target, up until its stdin closes.
    if std::env::args().nth(1).as_deref() == Some("linger") {
        std::io::stdin().read_to_end(&mut Vec::new()).expect("the linger child reads its stdin");
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
    // The child cannot end before its stdin closes, so the running answer is
    // asked of a child that is running whatever the clock says.
    let exe = std::env::current_exe().expect("current_exe failed");
    let mut child = Command::new(&exe)
        .arg("linger")
        .stdin(Stdio::piped())
        .spawn()
        .expect("spawn child failed");
    let running = child.try_wait().expect("try_wait failed");
    assert!(running.is_none(), "try_wait reported {running:?} for a child still reading its stdin");

    drop(child.stdin.take().expect("the child's piped stdin"));
    let status = loop {
        if let Some(status) = child.try_wait().expect("try_wait failed") {
            break status;
        }
        thread::sleep(POLL);
    };
    assert!(status.success(), "child exited with {status}");

    println!("all threading tests passed");
}
