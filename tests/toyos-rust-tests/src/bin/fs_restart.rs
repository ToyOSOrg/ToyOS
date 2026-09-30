//! A file server that ends is survived and not hidden.
//!
//! Booted by `common::storage::fsd_restart` on `tests/fsdrestartcase`, whose
//! file servers end the moment they take a write through a file opened to
//! append at `/home/fsd_end` and before they answer it, and at the first read
//! of an installed package's manifest and of its binary this boot:
//!
//! - a launch of an installed package is answered though DATA's server ends
//!   under init's read of its manifest and again under the read of its image:
//!   each call is init's file worker's, whose retry connects again and waits in
//!   the port's queue while init's loop starts the server again — a call from
//!   the loop would wait for ever on a server only the loop could start;
//! - the write the server ended under is answered as the server's end, never
//!   as done;
//! - a handle held across an end is answered `Gone`;
//! - a handle held across an end, on a file another was renamed over, is
//!   answered `Gone` and writes nothing into the file now at its path;
//! - DATA's server ended four times inside init's window is not started a
//!   fourth: `/home` then answers `Gone` to a new open and to a held handle.
//!
//! What is on the device is the host's to judge, off the image.

use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::process::Command;

/// Mirrored in `tests/common/storage.rs`.
const KEPT: &str = "/home/fs_restart/kept";
const ACROSS: &str = "/home/fs_restart/across";
const REPLACEMENT: &str = "/home/fs_restart/replacement";
const KEPT_LEN: usize = 64 * 1024 + 13;
const BEFORE: &[u8] = b"written and flushed before the server ended; ";
const REPLACING: &[u8] = b"renamed over the file a handle held across the end";

/// `--end-on`'s path, in `tests/fsdrestartcase/system.toml`.
const END: &str = "/home/fsd_end";

/// The package, whose manifest and binary are `--end-at-read`'s paths.
const PACKAGE: &str = "fs_restart";
const MANIFEST: &str = "/apps/fs_restart/manifest.toml";
const PROGRAM: &str = "/apps/fs_restart/fs_restart";
const SELF: &str = "/system/bin/test_rs_fs_restart";
/// What tells this binary it is the package's copy, launched.
const LAUNCHED: &str = "launched";

/// Mirrored: what `KEPT` holds.
fn kept() -> Vec<u8> {
    (0..KEPT_LEN).map(|i| (i.wrapping_mul(37) ^ 0xC3) as u8).collect()
}

/// End DATA's server: a write it takes and never answers.
fn end_the_server(n: u32) {
    let mut f = OpenOptions::new()
        .append(true)
        .create(true)
        .open(END)
        .unwrap_or_else(|e| panic!("end {n}: open {END} to append: {e}"));
    match f.write(b"the write the server ends under") {
        Err(e) if e.kind() == ErrorKind::StaleNetworkFileHandle => {
            println!("fs_restart: end {n}: the write was answered as the server's end ({e})");
        }
        Err(e) => panic!("end {n}: the write was refused with {e} ({:?}), not the server's end", e.kind()),
        Ok(n) => panic!("end {n}: the write the server ended under was answered done, {n} bytes"),
    }
}

/// This binary installed as a package, by writes alone — no read of either
/// file before init's — and durable, since an end loses what no sync covered.
fn install() {
    fs::create_dir_all(format!("/apps/{PACKAGE}")).expect("make the package's directory");
    fs::copy(SELF, PROGRAM).unwrap_or_else(|e| panic!("copy {SELF} to {PROGRAM}: {e}"));
    OpenOptions::new()
        .write(true)
        .open(PROGRAM)
        .and_then(|f| f.sync_all())
        .unwrap_or_else(|e| panic!("sync {PROGRAM}: {e}"));
    let manifest = format!(
        "name = \"{PACKAGE}\"\nversion = \"1\"\ndigest = \"{}\"\nprogram = \"{PROGRAM}\"\n",
        "0".repeat(64)
    );
    let mut f = File::create(MANIFEST).unwrap_or_else(|e| panic!("create {MANIFEST}: {e}"));
    f.write_all(manifest.as_bytes()).unwrap_or_else(|e| panic!("write {MANIFEST}: {e}"));
    f.sync_all().unwrap_or_else(|e| panic!("sync {MANIFEST}: {e}"));
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some(LAUNCHED) {
        println!("fs_restart: running from {PROGRAM}");
        return;
    }

    // Ends 1 and 2: init's reads of the manifest and of the image.
    install();
    let status =
        Command::new(PROGRAM).arg(LAUNCHED).status().unwrap_or_else(|e| panic!("launch {PROGRAM}: {e}"));
    assert!(status.success(), "{PROGRAM}, launched across two ends, exited {status}");
    println!("fs_restart: ends 1 and 2: the launch of {PROGRAM} was answered and it ran");

    fs::create_dir_all("/home/fs_restart").expect("make /home/fs_restart");
    let mut f = File::create(KEPT).expect("create the kept file");
    f.write_all(&kept()).expect("write the kept file");
    f.sync_all().expect("the kept file is durable");
    drop(f);
    let mut held = File::open(KEPT).expect("hold the kept file");
    let mut across = File::create(ACROSS).expect("create the file written across the end");
    across.write_all(BEFORE).expect("write before the end");
    across.sync_all().expect("durable before the end");
    // Another file renamed over the one `across` holds, durably.
    let mut replacement = File::create(REPLACEMENT).expect("create the replacement");
    replacement.write_all(REPLACING).expect("write the replacement");
    replacement.sync_all().expect("the replacement is durable");
    drop(replacement);
    fs::rename(REPLACEMENT, ACROSS).expect("rename the replacement over the held file");
    File::open(ACROSS).and_then(|f| f.sync_all()).expect("the rename is durable");

    end_the_server(3);

    match held.read(&mut [0u8; 16]) {
        Err(e) if e.kind() == ErrorKind::StaleNetworkFileHandle => {}
        other => panic!("a handle held across the end read {other:?}, not Gone"),
    }
    match across.write_all(b"written into whatever is at the path now") {
        Err(e) if e.kind() == ErrorKind::StaleNetworkFileHandle => {
            println!("fs_restart: a handle on a file renamed over is answered Gone ({e})");
        }
        Err(e) => panic!("a handle on a file renamed over was refused {e} ({:?}), not Gone", e.kind()),
        Ok(()) => panic!("a handle on a file renamed over wrote into the file now at its path"),
    }
    drop((held, across));
    let mut back = Vec::new();
    File::open(KEPT).and_then(|mut f| f.read_to_end(&mut back)).expect("a new open reads the kept file");
    assert!(back == kept(), "a new open read {} bytes, not the kept file", back.len());
    let mut whole = Vec::new();
    File::open(ACROSS).and_then(|mut f| f.read_to_end(&mut whole)).expect("read the file at the held path");
    assert_eq!(whole, REPLACING, "the file at the path a handle held across the end");
    println!("fs_restart: the server came back; held handles answered Gone, a new open read the flushed file");

    let mut held = File::open(KEPT).expect("hold the kept file for the last end");

    end_the_server(4);

    match File::open(KEPT) {
        Err(e) if e.kind() == ErrorKind::StaleNetworkFileHandle => {
            println!("fs_restart: after four ends a new open is answered Gone ({e})");
        }
        Err(e) => panic!("after four ends a new open was refused {e} ({:?}), not Gone", e.kind()),
        Ok(_) => panic!("after four ends a new open of {KEPT} was answered"),
    }
    match held.read(&mut [0u8; 16]) {
        Err(e) if e.kind() == ErrorKind::StaleNetworkFileHandle => {
            println!("fs_restart: and a held handle is answered Gone ({e})");
        }
        other => panic!("after four ends a held handle read {other:?}, not Gone"),
    }
    println!("fs_restart: PASS");
}
