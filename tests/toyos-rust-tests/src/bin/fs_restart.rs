//! A file server that ends is survived and not hidden.
//!
//! Booted by `common::storage::fsd_restart` on `tests/fsdrestartcase`, whose
//! file servers end the moment they take a write through a file opened to
//! append at `/home/fsd_end` and before they answer it:
//!
//! - a file written and `fsync`ed before the end — acknowledged and flushed —
//!   reads back through a handle held across it, and a handle written across
//!   it goes on writing where it was;
//! - the write the server ended under is answered as the server's end, never
//!   as done;
//! - DATA's server ended four times inside init's window is not started a
//!   fourth: `/home` then answers `Gone` to a new open and to a held handle.
//!
//! What is on the device is the host's to judge, off the image.

use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};

/// Mirrored in `tests/common/storage.rs`: the flushed file, and the one
/// written across the end.
const KEPT: &str = "/home/fs_restart/kept";
const ACROSS: &str = "/home/fs_restart/across";
const KEPT_LEN: usize = 64 * 1024 + 13;
const BEFORE: &[u8] = b"written and flushed before the server ended; ";
const AFTER: &[u8] = b"written through the same handle after it came back";

/// `--end-on`'s path, in `tests/fsdrestartcase/system.toml`.
const END: &str = "/home/fsd_end";

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
    fs::create_dir_all("/home/fs_restart").expect("make /home/fs_restart");
    let mut f = File::create(KEPT).expect("create the kept file");
    f.write_all(&kept()).expect("write the kept file");
    f.sync_all().expect("the kept file is durable");
    drop(f);
    let mut held = File::open(KEPT).expect("hold the kept file");
    let mut across = File::create(ACROSS).expect("create the file written across the end");
    across.write_all(BEFORE).expect("write before the end");
    across.sync_all().expect("durable before the end");

    end_the_server(1);

    let mut back = Vec::new();
    held.read_to_end(&mut back).expect("a handle held across the end reads");
    assert!(back == kept(), "the held handle read {} bytes, not the kept file", back.len());
    across.write_all(AFTER).expect("a handle held across the end writes where it was");
    across.sync_all().expect("durable after the end");
    let mut whole = Vec::new();
    File::open(ACROSS).and_then(|mut f| f.read_to_end(&mut whole)).expect("read the file written across");
    assert_eq!(whole, [BEFORE, AFTER].concat(), "the file written across the end");
    println!("fs_restart: the server came back; a held handle read, another wrote on, both where they were");

    for n in 2..=4 {
        end_the_server(n);
    }
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
