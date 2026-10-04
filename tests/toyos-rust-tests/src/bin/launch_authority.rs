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
//! - `toybox stats swap`: toybox's row lists `swap` and opens no session, so
//!   it asks in the session it was launched in, the machine's, and is refused;
//! - a shell, whose row opens a login session, running `toybox stats swap`:
//!   toybox asks in that session, and `swap` starts.
//!
//! `stats` exits 1 when its command does not start, and 0 once it has run.
//!
//! And a child it spawns directly holds no launcher, under its label or in
//! the namespace it inherits: the chain where any program a shell spawned
//! could ask for `swap` is gone.

use std::process::{Command, Stdio};

use toyos::endow::{self, EndowError, Endowments};
use toyos::launch::LAUNCHER;

const SELF_PATH: &str = "/system/bin/test_rs_launch_authority";
const STATS_SWAP: &str = "/system/bin/toybox stats /system/bin/swap";
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

    let from_machine = Command::new("/system/bin/toybox")
        .args(["stats", "/system/bin/swap"])
        .output()
        .expect("launch toybox as stats");
    match from_machine.status.code() {
        Some(1) => println!("  toybox in the machine's session: swap refused"),
        _ => red.push(format!("toybox launched in the machine's session started swap: {from_machine:?}")),
    }

    let from_login = Command::new("/system/bin/shell").args(["-c", STATS_SWAP]).output().expect("launch a shell");
    match from_login.status.code() {
        Some(0) => println!("  toybox in a login session: swap started"),
        _ => red.push(format!("toybox launched in a login session did not start swap: {from_login:?}")),
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
