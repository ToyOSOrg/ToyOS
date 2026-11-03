//! The kernel must survive a process that hands `SYS_SPAWN` absurd path, argv,
//! env or working directory, and refuse each at its boundary.
//!
//! `path_len`, `argv_len` and `env_len` are raw `u64` fields of a user-supplied
//! `SpawnArgs`. The path is copied, the argv blob is split into a `Vec<&str>`
//! (16 bytes per token) and the env blob is copied with `to_vec()`, so each one
//! sizes a kernel allocation. An argv with no non-empty token also has no `argv[0]`.
//! A working directory that is empty, relative or longer than `MAX_PATH` would
//! reach a child that cannot read it back.
//!
//! Every case carries a real image, and the same arguments with an honest argv
//! start a child, so each refusal is its own field's.

#[path = "../image.rs"]
mod image;

use std::process::Command;

use toyos::process::Process;
use toyos_abi::syscall::{self, VmAt, VmProt, SpawnArgs, SyscallError, MAX_PATH};

/// Where the child starts.
const CWD: &str = "/";

/// The program every case hands over, whatever its argv names.
const PROGRAM: &str = "/system/bin/echo";

/// One contiguous mmap region — `user_slice` requires physical contiguity,
/// so a heap `String` would risk a vacuous pass on `BadAddress`.
const REGION: usize = 4 * 1024 * 1024;
/// "a\0" repeated: 1,048,576 argv tokens at 16 bytes each.
const BLOB: usize = 2 * 1024 * 1024;

fn main() {
    let region = unsafe {
        syscall::vm_reserve(REGION, VmProt::ReadWrite, VmAt::ANYWHERE).unwrap_or(core::ptr::null_mut())
    };
    assert!(!region.is_null(), "mmap failed");
    for i in 0..BLOB / 2 {
        unsafe {
            region.add(i * 2).write(b'a');
            region.add(i * 2 + 1).write(0);
        }
    }

    let program = image::image_of(PROGRAM);
    let (image, image_len) = program.spawn_words();
    let base = SpawnArgs {
        path_ptr: region as u64,
        path_len: 1,
        argv_ptr: region as u64,
        argv_len: 0,
        env_ptr: 0,
        env_len: 0,
        endow_ptr: 0,
        endow_count: 0,
        labels_ptr: 0,
        labels_len: 0,
        cwd_ptr: CWD.as_ptr() as u64,
        cwd_len: CWD.len() as u64,
        image,
        image_len,
        place: u64::from(toyos_abi::HANDLE_INVALID.0),
        pager: u64::from(toyos_abi::HANDLE_INVALID.0),
    };

    let err = unsafe {
        syscall::spawn(&SpawnArgs { argv_len: BLOB as u64, ..base })
    }
    .expect_err("a 2 MiB argv blob must be rejected");
    assert_eq!(err, SyscallError::InvalidArgument, "wrong error for oversized argv");

    let err = unsafe {
        syscall::spawn(&SpawnArgs {
            argv_len: 2,
            env_ptr: region as u64,
            env_len: BLOB as u64,
            ..base
        })
    }
    .expect_err("a 2 MiB env blob must be rejected");
    assert_eq!(err, SyscallError::InvalidArgument, "wrong error for oversized env");

    let err = unsafe { syscall::spawn(&SpawnArgs { path_len: BLOB as u64, argv_len: 2, ..base }) }
        .expect_err("a 2 MiB path must be rejected");
    assert_eq!(err, SyscallError::InvalidArgument, "wrong error for oversized path");

    // The bound is a limit, not a ban: the same arguments with an honest
    // argv start a child.
    let child = unsafe { syscall::spawn(&SpawnArgs { argv_len: 2, ..base }) }.expect("an honest argv starts a child");
    // SAFETY: the handle `SYS_SPAWN` just answered, held nowhere else.
    unsafe { Process::from_raw(child) }.wait().expect("wait for the honest child");

    let relative = "tmp";
    let overlong = format!("/{}", "a".repeat(MAX_PATH));
    for (what, cwd) in [("an empty", ""), ("a relative", relative), ("an overlong", overlong.as_str())] {
        let err = unsafe {
            syscall::spawn(&SpawnArgs { argv_len: 2, cwd_ptr: cwd.as_ptr() as u64, cwd_len: cwd.len() as u64, ..base })
        }
        .expect_err("a working directory a child cannot read back must be refused");
        assert_eq!(err, SyscallError::InvalidArgument, "wrong error for {what} working directory");
    }

    // No non-empty token: there is no argv[0] to load.
    let err = unsafe { syscall::spawn(&base) }.expect_err("an empty argv must be rejected");
    assert_eq!(err, SyscallError::InvalidArgument, "wrong error for empty argv");

    // The tail of the region past the blob is still mmap's zero fill.
    let nuls = SpawnArgs { argv_ptr: region as u64 + BLOB as u64, argv_len: 8, ..base };
    let err = unsafe { syscall::spawn(&nuls) }.expect_err("an all-NUL argv must be rejected");
    assert_eq!(err, SyscallError::InvalidArgument, "wrong error for all-NUL argv");

    // Spawn still works, so the rejections left the kernel intact.
    let status = Command::new("/system/bin/echo")
        .arg("spawn still works")
        .status()
        .expect("spawn /system/bin/echo");
    assert!(status.success(), "/system/bin/echo exited {status:?}");

    unsafe { syscall::vm_release(region, REGION) }.expect("munmap");
    println!("oversized spawn path/argv/env and unreadable working directories rejected, spawn still usable");
}
