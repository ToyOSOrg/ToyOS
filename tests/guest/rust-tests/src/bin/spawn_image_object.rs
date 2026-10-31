//! A program handed to the kernel as a memory object is paged from that
//! object, and the object is the caller's to the end: what the kernel takes
//! from it is refused or safe, whatever the caller does with it.
//!
//! - A program read into an object of this process's own runs.
//! - A handle to the object without `MAP` is no right to its bytes: refused
//!   `PermissionDenied`. A length the object does not hold: `InvalidArgument`.
//! - The object overwritten with `hlt` the moment the spawn returns: the child
//!   runs what it had copied and faults on the rest, and the machine goes on —
//!   the next spawn from a fresh object runs.

use toyos::shm::SharedMemory;
use toyos::AsHandle;
use toyos_abi::handle::{RawHandle, Rights};
use toyos_abi::syscall::{self, SpawnArgs, SyscallError};
use toyos_abi::end::End;

const SELF: &str = "/system/bin/test_rs_spawn_image_object";
const CHILD: &str = "spawned-from-an-object";
const CWD: &str = "/";

/// `path` read whole into a memory object of this process's own.
fn object_of(path: &str) -> (SharedMemory, u64) {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    let object = SharedMemory::create(bytes.len()).expect("a memory object");
    object.bytes().write(0, &bytes);
    (object, bytes.len() as u64)
}

fn spawn(image: RawHandle, len: u64) -> Result<RawHandle, SyscallError> {
    let argv = format!("{SELF}\0{CHILD}\0");
    // SAFETY: every pointer names a live local for the whole call.
    unsafe {
        syscall::spawn(&SpawnArgs {
            path_ptr: SELF.as_ptr() as u64,
            path_len: SELF.len() as u64,
            argv_ptr: argv.as_ptr() as u64,
            argv_len: argv.len() as u64,
            env_ptr: 0,
            env_len: 0,
            endow_ptr: 0,
            endow_count: 0,
            labels_ptr: 0,
            labels_len: 0,
            cwd_ptr: CWD.as_ptr() as u64,
            cwd_len: CWD.len() as u64,
            image: image.0 as u64,
            image_len: len,
            place: u64::from(toyos_abi::HANDLE_INVALID.0),
            pager: u64::from(toyos_abi::HANDLE_INVALID.0),
        })
    }
}

fn runs(what: &str) {
    let (object, len) = object_of(SELF);
    let child = spawn(object.as_handle().raw(), len).unwrap_or_else(|e| panic!("{what}: spawn from an object: {e:?}"));
    drop(object);
    let end = syscall::process_wait(child).unwrap_or_else(|e| panic!("{what}: wait: {e:?}"));
    assert_eq!(end, End::Exited(0), "{what}: the child spawned from an object ended {end}");
    println!("spawn_image_object: {what}: a program in a memory object runs");
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some(CHILD) {
        return;
    }

    runs("first");

    let (object, len) = object_of(SELF);
    // SAFETY: nothing relies on being the source's only holder.
    let unmappable = unsafe { syscall::dup_narrowed(object.as_handle().raw(), Rights::DUP.union(Rights::TRANSFER)) }
        .expect("a duplicate without MAP");
    assert_eq!(spawn(unmappable, len).err(), Some(SyscallError::PermissionDenied), "an object without MAP");
    // SAFETY: no type holds this number.
    unsafe { syscall::close(unmappable) };
    // Past the whole object, which the kernel rounds up to 2 MiB pages.
    let past = (len | ((2 << 20) - 1)) + 2;
    assert_eq!(spawn(object.as_handle().raw(), past).err(), Some(SyscallError::InvalidArgument), "a length past the object");
    println!("spawn_image_object: an object without MAP and a length past one are refused");

    let child = spawn(object.as_handle().raw(), len).expect("spawn from the object that is then overwritten");
    // The kernel reads the object only by copying, which is what this races.
    object.bytes().write(0, &vec![0xF4; object.len()]);
    let end = syscall::process_wait(child).expect("the overwritten child is waited for");
    println!("spawn_image_object: a child whose object was overwritten after the spawn ended ({end})");
    drop(object);
    runs("after the overwrite");

    println!("spawn_image_object: PASS");
}
