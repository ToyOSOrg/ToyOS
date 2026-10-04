//! A launch starts only what the caller's row lists, and `swap` and `update`
//! only in a login session.
//!
//! This job holds test-runner's launcher, in the machine's session, and asks
//! the supervisor for:
//!
//! - `toybox` as `ps`, which test-runner's row lists: it runs under its row,
//!   which alone gives it the roster `ps` exits 0 with;
//! - `proctest`, which the row does not list, and `swap` and `update`, which
//!   it lists and which start only in a login session: each is refused, and
//!   nothing is spawned in its place;
//! - a shell, whose row opens a login session, that detaches `swap`: it
//!   starts there.
//!
//! And a child it spawns directly holds no launcher, under its label or in
//! the namespace it inherits: the chain where any program a shell spawned
//! could ask for `swap` is gone.
//!
//! Written against what the base it replaces had too, so the change reverted
//! reds here and does not fail to build. Which refusal each refused launch
//! got is in the supervisor's lines, which the metal judge reads.

use std::process::{Command, Stdio};

use toyos::endow::{self, EndowError, Endowments};

const SELF_PATH: &str = "/system/bin/test_rs_launch_authority";
const LAUNCHER: &str = "launcher";
const CHILD: &str = "child";
/// What the child exits with holding no launcher, and holding one.
const HOLDS_NONE: i32 = 0;
const HOLDS_ONE: i32 = 1;

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some(CHILD) => child(),
        _ => test(),
    }
}

fn test() {
    // Every arm runs, so one run says each one that is red.
    let mut red = Vec::new();

    let ps = Command::new("/system/bin/toybox").arg("ps").output().expect("launch toybox as ps");
    if ps.status.code() != Some(0) {
        red.push(format!("a listed row did not run under its row: {ps:?}"));
    }

    for program in ["/system/bin/proctest", "/system/bin/swap", "/system/bin/update"] {
        match Command::new(program).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
            Err(_) => println!("  {program}: refused"),
            Ok(mut started) => {
                let _ = started.kill();
                let _ = started.wait();
                red.push(format!("{program} started for a caller in the machine's session"));
            }
        }
    }

    // Piped, so the shell holds the input it hands what it detaches.
    let detached = Command::new("/system/bin/shell")
        .args(["-c", "detach /system/bin/swap"])
        .stdin(Stdio::piped())
        .output()
        .expect("launch a shell");
    if !detached.status.success() {
        red.push(format!("swap did not start in a login session: {detached:?}"));
    }

    let child = Command::new(SELF_PATH).arg(CHILD).status().expect("spawn this binary as a child");
    if child.code() != Some(HOLDS_NONE) {
        red.push(format!("a child spawned directly holds a launcher ({child:?})"));
    }

    assert!(red.is_empty(), "launch_authority:\n  {}", red.join("\n  "));
    println!("launch_authority: PASS");
}

fn child() -> ! {
    let labelled = Endowments::get().labels().any(|label| label == LAUNCHER);
    let inherited = !matches!(endow::service(LAUNCHER), Err(EndowError::NotEndowed));
    std::process::exit(if labelled || inherited { HOLDS_ONE } else { HOLDS_NONE })
}
