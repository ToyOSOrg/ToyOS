//! The kernel must survive a process that hands `SYS_SPAWN` an absurd path or
//! name, and refuse each at its boundary.
//!
//! `path_len` and `name_len` are raw `u64` fields of a user-supplied
//! `SpawnArgs`, and each is copied, so its length sizes a kernel allocation.
//! The kernel quotes the path in the spawn's record, so it is bounded text of
//! one line (`toyos_abi::syscall::spawn_path`): a path holding a line break
//! would put a line of its caller's writing among the kernel's records. The
//! name is printed in every record of the child, so it is one bounded token
//! (`toyos_abi::syscall::process_name`). An ask for isolated speculation is 0
//! or 1. The same arguments with an honest path and name start a child, so
//! each refusal is its own field's.

#[path = "../image.rs"]
mod image;

use toyos::process::Process;
use toyos_abi::syscall::{self, SpawnArgs, SyscallError, VmAt, VmProt, MAX_PATH, MAX_PROCESS_NAME};

const PROGRAM: &str = "/system/bin/echo";

/// A path whose second line reads as the kernel's record of a job's end.
const FORGED: &str = "/system/bin/echo\n[2026-11-11 10:00:00  1.000 cpu0 kernel] exit: forged pid=1 code=0 cpu=0ms";

/// One reservation, so that every byte of an absurd length is readable and
/// its refusal is not `BadAddress`.
const BLOB: usize = 2 * 1024 * 1024;

fn main() {
    // SAFETY: a fresh reservation nothing else names.
    let region = unsafe { syscall::vm_reserve(BLOB, VmProt::ReadWrite, VmAt::ANYWHERE) }.expect("a reservation");
    // SAFETY: `BLOB` writable bytes of the reservation.
    unsafe { region.write_bytes(b'a', BLOB) };

    let program = image::image_of(PROGRAM);
    let laid = image::laid(&program, PROGRAM);
    let honest = SpawnArgs {
        path_ptr: region as u64,
        path_len: MAX_PATH as u64,
        name_ptr: region as u64,
        name_len: MAX_PROCESS_NAME as u64,
        endow_ptr: 0,
        endow_count: 0,
        maps_ptr: laid.maps().as_ptr() as u64,
        maps_count: laid.maps().len() as u64,
        entry: laid.entry,
        image_base: laid.image_base,
        build_id_ptr: laid.build_id().as_ptr() as u64,
        build_id_len: laid.build_id().len() as u64,
        place: u64::from(toyos_abi::HANDLE_INVALID.0),
        job: u64::from(toyos_abi::HANDLE_INVALID.0),
        pager: u64::from(toyos_abi::HANDLE_INVALID.0),
        isolated_speculation: 0,
    };
    let pathed = |path: &[u8]| SpawnArgs { path_ptr: path.as_ptr() as u64, path_len: path.len() as u64, ..honest };
    let named = |name: &[u8]| SpawnArgs { name_ptr: name.as_ptr() as u64, name_len: name.len() as u64, ..honest };
    let refused: [(&str, SpawnArgs); 12] = [
        ("a 2 MiB path", SpawnArgs { path_len: BLOB as u64, ..honest }),
        ("a path one byte past the bound", SpawnArgs { path_len: MAX_PATH as u64 + 1, ..honest }),
        ("a path of two lines", pathed(FORGED.as_bytes())),
        ("a path holding an escape", pathed(b"/system/bin/\x1b[2Jecho")),
        ("a path that is not UTF-8", pathed(b"\xff")),
        ("a 2 MiB name", SpawnArgs { name_len: BLOB as u64, ..honest }),
        ("a name one byte past the bound", SpawnArgs { name_len: MAX_PROCESS_NAME as u64 + 1, ..honest }),
        ("no name", SpawnArgs { name_len: 0, ..honest }),
        ("a name of two lines", named(b"a\nb")),
        ("a name of two words", named(b"a b")),
        ("a name that is not UTF-8", named(b"\xff")),
        ("an ask that is neither 0 nor 1", SpawnArgs { isolated_speculation: 2, ..honest }),
    ];
    for (what, args) in refused {
        // SAFETY: every pointer names a live buffer for the whole call.
        let answer = unsafe { syscall::spawn(&args) };
        assert_eq!(answer, Err(SyscallError::InvalidArgument), "{what}");
        println!("abuse_spawn_path_and_name: {what}: refused");
    }

    // The bounds are limits: the longest path and name there are start a child.
    // SAFETY: as above.
    let child = unsafe { syscall::spawn(&honest) }.expect("the longest path and the longest name start a child");
    // SAFETY: the handle `SYS_SPAWN` just answered, held nowhere else.
    unsafe { Process::from_raw(child) }.wait().expect("wait for the honest child");

    // SAFETY: the reservation made above, which nothing names any more.
    unsafe { syscall::vm_release(region, BLOB) }.expect("release");
    println!("abuse_spawn_path_and_name: PASS");
}
