//! A file server that ends is survived and not hidden.
//!
//! Booted by `common::storage::fsd_restart` on `tests/fsdrestartcase`, whose
//! file servers end the moment they take a write through a file opened to
//! append at `/home/fsd_end` and before they answer it, and at the first hello
//! on `/apps` this boot:
//!
//! - a launch of an `/apps` path is answered though DATA's server ends while
//!   init resolves it: init makes that call on its file worker and starts the
//!   server again while it waits, where a call from its loop would wait for
//!   ever on a server only its loop could start;
//! - a file written and `fsync`ed before an end — acknowledged and flushed —
//!   reads back through a handle held across it, and a handle written across
//!   it goes on writing where it was;
//! - the write the server ended under is answered as the server's end, never
//!   as done;
//! - a handle held across an end, on a file another was renamed over, is
//!   answered `Gone` and writes nothing into the file now at its path;
//! - DATA's server ended four times inside init's window is not started a
//!   fourth: `/home` then answers `Gone` to a new open and to a held handle.
//!
//! What is on the device is the host's to judge, off the image.

use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::process::Command;

/// Mirrored in `tests/common/storage.rs`: the flushed file, the one written
/// across the first end, and what is renamed over it.
const KEPT: &str = "/home/fs_restart/kept";
const ACROSS: &str = "/home/fs_restart/across";
const REPLACEMENT: &str = "/home/fs_restart/replacement";
const KEPT_LEN: usize = 64 * 1024 + 13;
const BEFORE: &[u8] = b"written and flushed before the server ended; ";
const AFTER: &[u8] = b"written through the same handle after it came back";
const REPLACING: &[u8] = b"renamed over the file a handle held across the end";

/// `--end-on`'s path, in `tests/fsdrestartcase/system.toml`.
const END: &str = "/home/fsd_end";

/// A path under `--end-at-hello`'s directory: no package answers for it.
const LAUNCHED: &str = "/apps/fs_restart/nothing";

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

fn main() {
    // End 1: the first hello on `/apps` is init's, resolving this launch.
    match Command::new(LAUNCHED).status() {
        Err(e) => println!("fs_restart: end 1: the launch of {LAUNCHED} was answered, refused ({e})"),
        Ok(status) => panic!("{LAUNCHED}, which no package answers for, ran and exited {status}"),
    }

    fs::create_dir_all("/home/fs_restart").expect("make /home/fs_restart");
    let mut f = File::create(KEPT).expect("create the kept file");
    f.write_all(&kept()).expect("write the kept file");
    f.sync_all().expect("the kept file is durable");
    drop(f);
    let mut held = File::open(KEPT).expect("hold the kept file");
    let mut across = File::create(ACROSS).expect("create the file written across the end");
    across.write_all(BEFORE).expect("write before the end");
    across.sync_all().expect("durable before the end");

    end_the_server(2);

    let mut back = Vec::new();
    held.read_to_end(&mut back).expect("a handle held across the end reads");
    assert!(back == kept(), "the held handle read {} bytes, not the kept file", back.len());
    across.write_all(AFTER).expect("a handle held across the end writes where it was");
    across.sync_all().expect("durable after the end");
    let mut whole = Vec::new();
    File::open(ACROSS).and_then(|mut f| f.read_to_end(&mut whole)).expect("read the file written across");
    assert_eq!(whole, [BEFORE, AFTER].concat(), "the file written across the end");
    println!("fs_restart: the server came back; a held handle read, another wrote on, both where they were");

    // Another file renamed over the one `across` holds, durably, and then an end.
    let mut replacement = File::create(REPLACEMENT).expect("create the replacement");
    replacement.write_all(REPLACING).expect("write the replacement");
    replacement.sync_all().expect("the replacement is durable");
    drop(replacement);
    fs::rename(REPLACEMENT, ACROSS).expect("rename the replacement over the held file");
    File::open(ACROSS).and_then(|f| f.sync_all()).expect("the rename is durable");

    end_the_server(3);

    match across.write_all(b"written into whatever is at the path now") {
        Err(e) if e.kind() == ErrorKind::StaleNetworkFileHandle => {
            println!("fs_restart: a handle on a file renamed over is answered Gone ({e})");
        }
        Err(e) => panic!("a handle on a file renamed over was refused {e} ({:?}), not Gone", e.kind()),
        Ok(()) => panic!("a handle on a file renamed over wrote into the file now at its path"),
    }

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
