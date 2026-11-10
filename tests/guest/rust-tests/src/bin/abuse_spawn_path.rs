//! The kernel must survive a process that hands `SYS_SPAWN` an absurd path,
//! and refuse it at its boundary.
//!
//! `path_len` is a raw `u64` field of a user-supplied `SpawnArgs`, and the
//! path is copied, so it sizes a kernel allocation. The same arguments with
//! an honest path start a child, so the refusal is the path's.

#[path = "../image.rs"]
mod image;

use toyos::process::Process;
use toyos_abi::syscall::{self, SpawnArgs, SyscallError, VmAt, VmProt};

const PROGRAM: &str = "/system/bin/echo";

/// One reservation, so that every byte of the absurd path is readable and its
/// refusal is not `BadAddress`.
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
        path_len: 1,
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

    // SAFETY: every pointer names a live buffer for the whole call.
    let err = unsafe { syscall::spawn(&SpawnArgs { path_len: BLOB as u64, ..honest }) }.expect_err("a 2 MiB path must be rejected");
    assert_eq!(err, SyscallError::InvalidArgument, "wrong error for an oversized path");

    // SAFETY: as above.
    let child = unsafe { syscall::spawn(&honest) }.expect("an honest path starts a child");
    // SAFETY: the handle `SYS_SPAWN` just answered, held nowhere else.
    unsafe { Process::from_raw(child) }.wait().expect("wait for the honest child");

    // SAFETY: the reservation made above, which nothing names any more.
    unsafe { syscall::vm_release(region, BLOB) }.expect("release");
    println!("an oversized spawn path is rejected, and spawn is still usable");
}
