//! A region handed out without `WRITE` is the writer's alone to write: the
//! holder maps it readable and sees the writer's bytes, is refused a writable
//! mapping, a widening `vm_protect` and a writable part of it by word, and is
//! ended for a store through what it mapped.

use std::io::Write;
use std::os::toyos::process::ExitStatusExt;
use std::process::{Command, Stdio};

use toyos::shm::{Lent, ReadOnly, SharedMemory};
use toyos::{AsHandle, OwnedHandle};
use toyos_abi::end::{End, Fault};
use toyos_abi::handle::Rights;
use toyos_abi::syscall::{self, SyscallError, VmProt};

const SELF_PATH: &str = "/system/bin/test_rs_shm_read_only";
const STORES: &str = "stores";
const REGION: usize = 4096;
const SAID: &[u8] = b"written by the one writer";

/// What a region's handle carries, but `WRITE`.
const READ_ONLY: Rights = Rights::MAP.union(Rights::DUP).union(Rights::TRANSFER);

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some(STORES) => stores(),
        Some(other) => panic!("shm_read_only: unknown role {other:?}"),
        None => holder(),
    }
}

fn holder() {
    let (reader, writer) = ReadOnly::create(REGION).expect("shm_read_only: a region and its writer");
    let writer = SharedMemory::lent(writer, REGION).expect("shm_read_only: the writer maps its handle");
    writer.bytes().write(0, SAID);
    assert_eq!(reader.bytes().sub(0, SAID.len()).to_vec(), SAID, "shm_read_only: the reader does not see the writer's bytes");

    // SAFETY: nothing relies on being the source's only holder.
    let narrowed = unsafe { syscall::dup_narrowed(writer.as_handle().raw(), READ_ONLY) }.expect("shm_read_only: a read-only handle");
    // SAFETY: the mapping is never reached, and is the process's to the end.
    let writable = unsafe { syscall::shm_map(narrowed, VmProt::ReadWrite) }.map(|_| ());
    assert_eq!(writable, Err(SyscallError::PermissionDenied), "shm_read_only: a handle without WRITE mapped writable");

    // SAFETY: as above.
    let at = unsafe { syscall::shm_map(narrowed, VmProt::Read) }.expect("shm_read_only: a read-only mapping");
    // SAFETY: `at` is a mapping of its own this process never takes away.
    let widened = unsafe { syscall::vm_protect(at, REGION, VmProt::ReadWrite) };
    assert_eq!(widened, Err(SyscallError::PermissionDenied), "shm_read_only: vm_protect made a read-only mapping writable");

    // SAFETY: nothing relies on being the source's only holder.
    let part = unsafe { syscall::shm_sub(narrowed, 0, REGION as u64) }.expect("shm_read_only: part of the region");
    // SAFETY: as above.
    let part_writable = unsafe { syscall::shm_map(part, VmProt::ReadWrite) }.map(|_| ());
    assert_eq!(part_writable, Err(SyscallError::PermissionDenied), "shm_read_only: part of a read-only region mapped writable");

    // SAFETY: nothing relies on being the source's only holder.
    let widen = unsafe { syscall::dup_narrowed(narrowed, READ_ONLY.union(Rights::WRITE)) };
    assert_eq!(widen, Err(SyscallError::PermissionDenied), "shm_read_only: a duplicate widened a read-only handle");

    // SAFETY: a fresh handle nothing else answers for.
    let lent = SharedMemory::lent(unsafe { OwnedHandle::from_raw(narrowed) }, REGION).err();
    assert_eq!(lent, Some(Lent::Kernel(SyscallError::PermissionDenied)), "shm_read_only: a read-only handle lent as a writable region");

    let child = Command::new(SELF_PATH).arg(STORES).stdout(Stdio::piped()).spawn().expect("shm_read_only: spawn the storer");
    let out = child.wait_with_output().expect("shm_read_only: wait the storer");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "reached stores", "shm_read_only: the storer never reached its store");
    assert_eq!(
        End::from_word(out.status.into_raw()),
        Some(End::Fault(Fault::Memory)),
        "shm_read_only: a store through a read-only mapping was not a memory fault"
    );
    println!("shm_read_only: a region without WRITE maps only readable, and nothing it is narrowed or protected to writes it");
}

/// Stores through a read-only mapping of a region of its own.
fn stores() -> ! {
    let writer = syscall::shm_create(REGION).expect("shm_read_only: a region to store into");
    // SAFETY: nothing relies on being the source's only holder.
    let narrowed = unsafe { syscall::dup_narrowed(writer, READ_ONLY) }.expect("shm_read_only: a read-only handle");
    // SAFETY: the mapping is this process's to the end, which the store below is.
    let at = unsafe { syscall::shm_map(narrowed, VmProt::Read) }.expect("shm_read_only: a read-only mapping");
    println!("reached {STORES}");
    std::io::stdout().flush().expect("shm_read_only: flush the marker");
    // SAFETY: none is owed: the store is the fault this role exists to take.
    unsafe { core::ptr::write_volatile(at, 1) };
    panic!("shm_read_only: a store through a read-only mapping landed");
}
