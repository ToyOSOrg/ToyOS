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
    match std::env::args().nth(1).as_deref() {
        Some("symlink-row") => return symlink_row(),
        Some("relative-path") => return relative_path(),
        _ => {}
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

/// The first is what `package_of` classifies; the other four it answers `None`
/// for while `sys_readlink` lands them all on the same file.
const SPELLINGS: [&str; 5] = [
    "/apps/toy/echo",
    "/apps/./toy/echo",
    "/apps//toy/echo",
    "apps/toy/echo",
    "/tmp/../apps/toy/echo",
];

/// `toybox`'s row carries `syscap = ["power"]` here and this estate holds none,
/// so a launch resolving through the link is a process handed a capability
/// nothing gave it. Exit 0 is every spelling refused.
fn symlink_row() -> std::process::ExitCode {
    std::fs::create_dir_all(PLANTED_DIR).expect("/apps is writable");
    toyos_abi::syscall::symlink(DECLARED.as_bytes(), PLANTED.as_bytes())
        .expect("a symlink under /apps is allowed");
    for spelling in SPELLINGS {
        let target = std::fs::read_link(spelling).expect("every spelling reaches the link");
        assert_eq!(target.to_str(), Some(DECLARED), "{spelling} does not reach the planted link");
    }

    let mut refused = 0;
    for spelling in SPELLINGS {
        match Command::new(spelling).spawn() {
            Ok(child) => println!(
                "pkg-symlink: {spelling} started as pid {} — a link under /apps reached \
                 {DECLARED}'s row",
                child.id()
            ),
            Err(e) => {
                println!("pkg-symlink: {spelling} refused: {e}");
                refused += 1;
            }
        }
    }
    if refused == SPELLINGS.len() {
        std::process::ExitCode::SUCCESS
    } else {
        println!("pkg-symlink: {refused} of {} spellings refused", SPELLINGS.len());
        std::process::ExitCode::FAILURE
    }
}

/// The real shell binary through `shell -c`, rather than a second copy of what
/// it does. `-c` roots the cwd at `/`, so the dotted forms are rooted there.
fn relative_path() -> std::process::ExitCode {
    const DIR: &str = "/home/root/reltest";
    const NONCE: &str = "relpath-ran-9c41";
    std::fs::create_dir_all(DIR).expect("/home is writable");
    let link = format!("{DIR}/echo");
    let _ = std::fs::remove_file(&link);
    toyos_abi::syscall::symlink(DECLARED.as_bytes(), link.as_bytes())
        .expect("a symlink under /home is allowed");

    let mut ran = 0;
    for typed in ["./home/root/reltest/echo", "../home/root/reltest/echo"] {
        let out = Command::new("/system/bin/shell")
            .arg("-c")
            .arg(format!("{typed} {NONCE}"))
            .output();
        match out {
            Ok(out) => {
                let said = String::from_utf8_lossy(&out.stdout).into_owned();
                if said.contains(NONCE) {
                    println!("pkg-relpath: {typed} ran and said {NONCE}");
                    ran += 1;
                } else {
                    println!("pkg-relpath: {typed} said {said:?}, not {NONCE}");
                }
            }
            Err(e) => println!("pkg-relpath: the shell did not start: {e}"),
        }
    }
    if ran == 2 {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}
