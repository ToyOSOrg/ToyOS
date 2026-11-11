//! A child reads what its parent laid out for it, and nothing the kernel
//! kept: its arguments and environment are bytes of the startup block
//! (`toyos::startup`), so they are whole past any bound a kernel copy had,
//! and a child whose parent laid out no block starts with neither.
//!
//! A guest test because the block is read at an address of the child's own
//! space, through the pages the kernel copies at their first touch.

#[path = "../image.rs"]
mod image;

use std::process::Command;

use toyos::elf::program::lay_out;
use toyos::elf::Machine;
use toyos_abi::end::End;
use toyos_abi::syscall::{self, SpawnArgs};

const SELF: &str = "/system/bin/test_rs_startup_block";
/// The name its children go by.
const NAME: &str = "test_rs_startup_block";
const SAID: &str = "said";
/// What each child exits with once its checks hold, so neither can pass as the other.
const SAID_HELD: i32 = 41;
const UNSAID_HELD: i32 = 42;
/// Past the 64 KiB the kernel once copied of each, and past a page many times over.
const LONG: usize = 300 * 1024;

fn long(byte: u8) -> String {
    String::from_utf8(vec![byte; LONG]).expect("ASCII")
}

/// The child given no block: no argument, not even its name, and no variable.
fn unsaid() -> ! {
    assert_eq!(std::env::vars_os().count(), 0, "a child given no block has an environment");
    std::process::exit(UNSAID_HELD);
}

/// The child given the parent's block: each argument and variable whole.
fn said() -> ! {
    let args: Vec<String> = std::env::args().collect();
    assert_eq!(args.len(), 4, "arguments: {}", args.len());
    assert_eq!((args[0].as_str(), args[1].as_str(), args[3].as_str()), (SELF, SAID, "last"));
    assert!(args[2] == long(b'a'), "the long argument is {} bytes, or not the parent's", args[2].len());
    assert!(std::env::var("LONG").is_ok_and(|v| v == long(b'e')), "the long variable is not the parent's");
    assert_eq!(std::env::var("AFTER").as_deref(), Ok("the variable after it"));
    assert_eq!(std::env::current_dir().expect("a working directory").to_str(), Some("/tmp"));
    std::process::exit(SAID_HELD);
}

/// This program started from its ranges alone, as `lay_out` leaves them.
fn spawn_unsaid() -> End {
    let own = image::image_of(SELF);
    let laid = lay_out(own.bytes(), Machine::RUNNING, own.source()).expect("this program lays out");
    let args = SpawnArgs {
        path_ptr: SELF.as_ptr() as u64,
        path_len: SELF.len() as u64,
        name_ptr: NAME.as_ptr() as u64,
        name_len: NAME.len() as u64,
        endow_ptr: 0,
        endow_count: 0,
        labels_ptr: 0,
        labels_len: 0,
        maps_ptr: laid.maps().as_ptr() as u64,
        maps_count: laid.maps().len() as u64,
        entry: laid.entry,
        image_base: laid.image_base,
        build_id_ptr: laid.build_id().as_ptr() as u64,
        build_id_len: laid.build_id().len() as u64,
        place: u64::from(toyos_abi::HANDLE_INVALID.0),
        pager: u64::from(toyos_abi::HANDLE_INVALID.0),
    };
    // SAFETY: every pointer names a live local for the whole call.
    let child = unsafe { syscall::spawn(&args) }.expect("a spawn with no block");
    syscall::process_wait(child).expect("wait for the child given no block")
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        _ if args.is_empty() => unsaid(),
        Some(SAID) => said(),
        _ => {}
    }
    let status = Command::new(SELF)
        .args([SAID, &long(b'a'), "last"])
        .env("LONG", long(b'e'))
        .env("AFTER", "the variable after it")
        .current_dir("/tmp")
        .status()
        .expect("spawn the child given a block");
    assert_eq!(status.code(), Some(SAID_HELD), "the child given a block: {status:?}");
    assert_eq!(spawn_unsaid(), End::Exited(UNSAID_HELD), "the child given no block");
    println!("startup_block: PASS");
}
