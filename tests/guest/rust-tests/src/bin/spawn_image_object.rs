//! A program handed to the kernel as a memory object is paged from that
//! object, and the object is the caller's to the end: what the kernel takes
//! from it is refused or safe, whatever the caller does with it.
//!
//! - A program read into an object of this process's own runs.
//! - A handle to the object without `MAP` is no right to its bytes: refused
//!   `PermissionDenied`. A range's other refusals are `abuse_spawn_maps`'s.
//! - The object overwritten with `hlt` the moment the spawn returns: the child
//!   runs what it had copied and faults on the rest, and the machine goes on —
//!   the next spawn from a fresh object runs.

use toyos::elf::program::{lay_out, Source};
use toyos::elf::Machine;
use toyos::shm::SharedMemory;
use toyos::startup::Block;
use toyos::AsHandle;
use toyos_abi::handle::{RawHandle, Rights};
use toyos_abi::syscall::{self, SpawnArgs, SyscallError};
use toyos_abi::end::End;

const SELF: &str = "/system/bin/test_rs_spawn_image_object";
/// The name its children go by.
const NAME: &str = "test_rs_spawn_image_object";
const CHILD: &str = "spawned-from-an-object";

/// `path` read whole into a memory object of this process's own, and its bytes.
fn object_of(path: &str) -> (SharedMemory, Vec<u8>) {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    let object = SharedMemory::create(bytes.len()).expect("a memory object");
    object.bytes().write(0, &bytes);
    (object, bytes)
}

/// Spawn the program `bytes`, laid out from the object `image` names.
fn spawn(image: RawHandle, bytes: &[u8]) -> Result<RawHandle, SyscallError> {
    let block = Block::new(format!("{SELF}\0{CHILD}").as_bytes(), b"", &[], None).expect("a startup block");
    let laid = lay_out(bytes, Machine::RUNNING, Source::Copied(u64::from(image.0))).expect("this program lays out").starting_with(&block);
    // SAFETY: every pointer names a live local for the whole call.
    unsafe {
        syscall::spawn(&SpawnArgs {
            path_ptr: SELF.as_ptr() as u64,
            path_len: SELF.len() as u64,
            name_ptr: NAME.as_ptr() as u64,
            name_len: NAME.len() as u64,
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
        })
    }
}

fn runs(what: &str) {
    let (object, bytes) = object_of(SELF);
    let child = spawn(object.as_handle().raw(), &bytes).unwrap_or_else(|e| panic!("{what}: spawn from an object: {e:?}"));
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

    let (object, bytes) = object_of(SELF);
    // SAFETY: nothing relies on being the source's only holder.
    let unmappable = unsafe { syscall::dup_narrowed(object.as_handle().raw(), Rights::DUP.union(Rights::TRANSFER)) }
        .expect("a duplicate without MAP");
    assert_eq!(spawn(unmappable, &bytes).err(), Some(SyscallError::PermissionDenied), "an object without MAP");
    // SAFETY: no type holds this number.
    unsafe { syscall::close(unmappable) };
    println!("spawn_image_object: an object without MAP is refused");

    let child = spawn(object.as_handle().raw(), &bytes).expect("spawn from the object that is then overwritten");
    // The kernel reads the object only by copying, which is what this races.
    object.bytes().write(0, &vec![0xF4; object.len()]);
    let end = syscall::process_wait(child).expect("the overwritten child is waited for");
    println!("spawn_image_object: a child whose object was overwritten after the spawn ended ({end})");
    drop(object);
    runs("after the overwrite");

    println!("spawn_image_object: PASS");
}
