//! A spawn and the end of its child leave every kind of kernel object as it
//! was, for a program from each of three places: a file its server holds in
//! memory, which it answers as a region over those pages; a file a server
//! does not, which the spawn reads whole into a region of the spawner's own;
//! and a file of the kernel's `/tmp`, which the kernel opens itself. Whatever
//! the child is paged from, the spawner and the child must both let it go.
//!
//! Roles: no argument is the test; `held` writes a line and waits for its
//! stdin to end.

use std::io::{Read, Write};
use std::process::{Command, Stdio};

use toyos::census::Census;
use toyos_abi::handle::ObjectKind;

#[path = "../census_wait.rs"]
mod census_wait;
#[path = "../image.rs"]
mod image;

const SELF_PATH: &str = "/system/bin/test_rs_spawn_returns_the_census";
/// The test runner's own directory on DATA, which every job of it inherits.
const STATE: &str = "/state/test-runner";
/// Spawns per source: a kept object is one of these each.
const ROUNDS: usize = 8;

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("held") => held(),
        Some(other) => panic!("unknown role {other:?}"),
        None => {
            spawns_return_the_census("the served /system", SELF_PATH);
            let copy = in_state(SELF_PATH);
            spawns_return_the_census("a copy in /state", &copy);
            std::fs::remove_file(&copy).unwrap_or_else(|e| panic!("remove {copy}: {e}"));
            spawns_return_the_census("a copy in /tmp", &image::in_tmp(SELF_PATH));
        }
    }
}

/// [`ROUNDS`] children of `path`, one at a time, then a wait for the census
/// to come back to its first reading, which panics naming a kind still grown.
fn spawns_return_the_census(source: &str, path: &str) {
    // Opened once before the first reading: this process's connection to
    // the file's server, which it keeps, is not a spawn's.
    drop(std::fs::File::open(path).unwrap_or_else(|e| panic!("open {path}: {e}")));
    let before = census_wait::settled();
    for _ in 0..ROUNDS {
        // Its stdout is this process's log ring, so the child maps a region
        // that outlives it.
        let mut child = Command::new(path)
            .arg("held")
            .stdin(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("spawn {path}: {e}"));

        // Non-vacuity: a census that cannot see the child and what it is paged
        // from arrive cannot see them leave either.
        let live = Census::now();
        for kind in [ObjectKind::Process, ObjectKind::SharedMem] {
            assert!(
                live.kind(kind) > before.kind(kind),
                "a live child of {path} moved no {kind:?}: first {before}, then {live}"
            );
        }

        drop(child.stdin.take());
        let status = child.wait().expect("wait for the child");
        assert!(status.success(), "the child of {path} ended {status:?}");
        // Dropped before the reading: a reaped `Child` still names its process.
        drop(child);
    }
    census_wait::released_to(&before);
    println!("{ROUNDS} spawns from {source}: the census came back to its first reading");
}

/// `path` copied into [`STATE`] under its own name: the path to spawn.
fn in_state(path: &str) -> String {
    let name = path.rsplit('/').next().expect("a path names a file");
    let copy = format!("{STATE}/{name}");
    std::fs::copy(path, &copy).unwrap_or_else(|e| panic!("copy {path} to {copy}: {e}"));
    copy
}

fn held() {
    println!("held");
    std::io::stdout().flush().expect("held: flush");
    let mut rest = Vec::new();
    std::io::stdin().read_to_end(&mut rest).expect("held: read stdin to its end");
}
