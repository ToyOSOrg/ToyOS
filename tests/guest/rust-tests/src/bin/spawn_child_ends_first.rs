//! A spawn whose child has ended before the spawn answers still answers a
//! handle to it, and the handle reads the child's code.
//!
//! A child's own table holds its `self` and closes when the child ends, so a
//! spawner's handle minted after that would be the first on an object whose
//! last had gone. No caller can order a child's end inside the spawn that
//! starts it: `debug_action::HOLD_SPAWN_UNTIL_CHILD_ENDS` has the kernel hold
//! this process's next spawn, once its child has landed, until the child's
//! exit is published.
//!
//! The same hold is what lets a refused spawn be asked whether it started a
//! child: one that had landed would have run to its end, and spoken, before
//! the spawn answered.
//!
//! Every wait is unbounded: the runner's deadline is the only clock.

use toyos_abi::syscall::{self, debug_action, SpawnArgs, SyscallError};
use toyos_abi::RawHandle;

const SELF_PATH: &str = "/system/bin/test_rs_spawn_child_ends_first";

/// What the child exits with.
const CODE: i32 = 7;

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("exit") => std::process::exit(CODE),
        Some("speak") => {
            assert_eq!(syscall::write(RawHandle(1), b"x"), Ok(1), "the child's one byte");
        }
        Some(other) => panic!("unknown role {other:?}"),
        None => test(),
    }
}

fn test() {
    hold_the_next_spawn();
    let child = spawn("exit", &[]).expect("a spawn whose child ended inside it");
    assert_eq!(
        syscall::process_wait_nonblock(child),
        Ok(CODE),
        "the child had not ended when its spawn answered, so the kernel held nothing"
    );
    syscall::close(child);
    a_spawn_from_a_full_table_starts_no_child();
    println!("spawn_child_ends_first: the spawn answered a child that had already ended, and a refused one started none");
}

/// A spawn whose caller's table has no slot for its answer is refused with
/// nothing of the child started: the pipe its child would have spoken into
/// ends empty. The mark is left standing, since the spawn never lands.
fn a_spawn_from_a_full_table_starts_no_child() {
    let ends = syscall::pipe().expect("a pipe for the child to speak into");
    let mut filled = Vec::new();
    let full = loop {
        match syscall::dup(ends.read) {
            Ok(handle) => filled.push(handle),
            Err(refused) => break refused,
        }
    };
    assert_eq!(full, SyscallError::ResourceExhausted, "the table did not fill");
    hold_the_next_spawn();
    let spawned = spawn("speak", &[[1, ends.write.0]]);
    for handle in filled {
        syscall::close(handle);
    }
    // Judged once the table has room again: a panic at the cap aborts with no report.
    assert_eq!(spawned, Err(SyscallError::ResourceExhausted), "a spawn from a full table was not refused");
    syscall::close(ends.write);
    let mut byte = [0u8; 1];
    assert_eq!(syscall::read(ends.read, &mut byte), Ok(0), "the refused spawn's child started and spoke");
    syscall::close(ends.read);
}

fn hold_the_next_spawn() {
    assert_eq!(
        syscall::debug(debug_action::HOLD_SPAWN_UNTIL_CHILD_ENDS),
        0,
        "this kernel does not carry SYS_DEBUG, so nothing holds the spawn"
    );
}

/// `SYS_SPAWN` of this binary in `role`, duplicating each `[child_slot,
/// parent_handle]` pair of `slot_map` into the child.
fn spawn(role: &str, slot_map: &[[u32; 2]]) -> Result<RawHandle, SyscallError> {
    let argv = format!("{SELF_PATH}\0{role}");
    let args = SpawnArgs {
        path_ptr: SELF_PATH.as_ptr() as u64,
        path_len: SELF_PATH.len() as u64,
        argv_ptr: argv.as_ptr() as u64,
        argv_len: argv.len() as u64,
        slot_map_ptr: slot_map.as_ptr() as u64,
        slot_map_count: slot_map.len() as u64,
        env_ptr: 0,
        env_len: 0,
        endow_ptr: 0,
        endow_count: 0,
        labels_ptr: 0,
        labels_len: 0,
        cwd_ptr: "/".as_ptr() as u64,
        cwd_len: 1,
        image: 0,
        image_len: 0,
        place: u64::from(toyos_abi::HANDLE_INVALID.0),
    };
    // SAFETY: every pointer names a buffer of this frame that outlives the call.
    unsafe { syscall::spawn(&args) }
}
