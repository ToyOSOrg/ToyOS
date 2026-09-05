//! Launch an installed package, holding nothing that could start it otherwise.
//!
//! This binary is no `[programs]` key, so it holds what `test-runner` holds,
//! and `tests/pkgcase/system.toml` gives that estate a `launcher` connector and
//! no `compositor` one. A window that appears after this came from the `[apps]`
//! row init built out of `/apps/gbae/manifest.toml`, because inheritance
//! carries nothing that would draw one.
//!
//! It does not wait: gbae runs until the machine goes down, and the compositor
//! census on the host's side of the serial says the window exists.

use std::process::Command;

const PROGRAM: &str = "/apps/gbae/gbae";

const PLANTED_DIR: &str = "/apps/toy";
const PLANTED: &str = "/apps/toy/echo";
const DECLARED: &str = "/system/bin/toybox";

fn main() -> std::process::ExitCode {
    if std::env::args().nth(1).as_deref() == Some("symlink-row") {
        return symlink_row();
    }
    // The refusal arm is a first-class outcome and not a panic: the same
    // binary runs before the package is installed and after it is removed,
    // where init answering "no" is the assertion.
    match Command::new(PROGRAM).spawn() {
        Ok(child) => {
            println!("pkg-launch: started {PROGRAM} as pid {}", child.id());
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            println!("pkg-launch: {PROGRAM} did not start: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// `toybox`'s row carries `syscap = ["power"]` here and this estate holds none,
/// so a launch resolving through the link is a process handed a capability
/// nothing gave it. Exit 0 is the refusal.
fn symlink_row() -> std::process::ExitCode {
    std::fs::create_dir_all(PLANTED_DIR).expect("/apps is writable");
    toyos_abi::syscall::symlink(DECLARED.as_bytes(), PLANTED.as_bytes())
        .expect("a symlink under /apps is allowed");
    let target = std::fs::read_link(PLANTED).expect("the link was planted");
    assert_eq!(target.to_str(), Some(DECLARED), "the link does not point where it was aimed");

    match Command::new(PLANTED).spawn() {
        Ok(child) => {
            println!(
                "pkg-symlink: {PLANTED} started as pid {} — a link under /apps reached {DECLARED}'s row",
                child.id()
            );
            std::process::ExitCode::FAILURE
        }
        Err(e) => {
            println!("pkg-symlink: {PLANTED} refused: {e}");
            std::process::ExitCode::SUCCESS
        }
    }
}
