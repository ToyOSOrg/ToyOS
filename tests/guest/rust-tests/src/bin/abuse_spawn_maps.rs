//! The kernel builds a child from the ranges its parent laid out, and parses
//! no program: each range is checked as an ordinary mapping is, all or
//! nothing. A range this refuses is one the child's space could not safely
//! hold, so every refusal leaves the caller and the machine as they were, and
//! the next honest spawn runs.

#[path = "../image.rs"]
mod image;

use toyos::process::Image;
use toyos_abi::end::End;
use toyos_abi::handle::Rights;
use toyos_abi::syscall::{self, SpawnArgs, SpawnMap, SyscallError, VmProt, MAX_SPAWN_MAPS};
use toyos_abi::RawHandle;

const SELF: &str = "/system/bin/test_rs_abuse_spawn_maps";
const CHILD: &str = "spawned-from-checked-ranges";
const NONE: u64 = toyos_abi::HANDLE_INVALID.0 as u64;

/// Spawn this program with `maps` and `entry`, its image laid out otherwise.
fn spawn(maps: &[SpawnMap], entry: u64, image_base: u64) -> Result<RawHandle, SyscallError> {
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
            maps_ptr: maps.as_ptr() as u64,
            maps_count: maps.len() as u64,
            entry,
            image_base,
            build_id_ptr: 0,
            build_id_len: 0,
            place: u64::from(toyos_abi::HANDLE_INVALID.0),
            pager: u64::from(toyos_abi::HANDLE_INVALID.0),
        })
    }
}

fn runs(what: &str, image: &Image) {
    let laid = image::laid(image);
    let child = spawn(laid.maps(), laid.entry, laid.image_base).unwrap_or_else(|e| panic!("{what}: an honest spawn: {e:?}"));
    let end = syscall::process_wait(child).unwrap_or_else(|e| panic!("{what}: wait: {e:?}"));
    assert_eq!(end, End::Exited(0), "{what}: the child ended {end}");
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some(CHILD) {
        return;
    }
    let image = image::image_of(SELF);
    let laid = image::laid(&image);
    runs("first", &image);
    let honest = laid.maps().to_vec();
    let text = honest.iter().position(|m| m.prot == VmProt::ReadExec as u64).expect("an executable range");
    let refused = |what: &str, maps: Vec<SpawnMap>, entry: u64, want: SyscallError| {
        assert_eq!(spawn(&maps, entry, laid.image_base).err(), Some(want), "{what}");
        println!("abuse_spawn_maps: {what}: refused {want:?}");
    };
    let with = |edit: &dyn Fn(&mut Vec<SpawnMap>)| {
        let mut maps = honest.clone();
        edit(&mut maps);
        maps
    };
    let invalid = SyscallError::InvalidArgument;

    refused("no ranges", Vec::new(), laid.entry, invalid);
    refused("more ranges than a spawn carries", vec![honest[0]; MAX_SPAWN_MAPS + 1], laid.entry, invalid);
    refused("an address that is no page boundary", with(&|m| m[0].addr += 8), laid.entry, invalid);
    refused("a length that is no whole pages", with(&|m| m[0].len += 8), laid.entry, invalid);
    refused("a range past the user half", with(&|m| m[0].addr = 0x7fff_ffff_f000), laid.entry, invalid);
    refused("a range in the kernel half", with(&|m| m[0].addr = 0xffff_8000_0000_0000), laid.entry, invalid);
    refused("a protection no VmProt names", with(&|m| m[text].prot = 4), laid.entry, invalid);
    refused("more bytes copied than the range holds", with(&|m| m[0].copied = m[0].len + 1), laid.entry, invalid);
    refused("a copy past the object's end", with(&|m| m[0].offset = 0x4000_0000), laid.entry, invalid);
    refused("a copy from no object", with(&|m| m[0].object = NONE), laid.entry, invalid);
    refused("a copy from no page boundary of the object", with(&|m| m[text].offset += 8), laid.entry, invalid);
    // Unchecked, the sum wraps past the object's size and the first touch
    // reads past the object's end in the kernel.
    refused("a copy whose end wraps", with(&|m| (m[text].offset, m[text].copied) = (0xffff_ffff_ffff_f000, 0x1000)), laid.entry, invalid);
    refused("a range on the clock page", with(&|m| m.push(SpawnMap { addr: toyos_abi::clock::CLOCK_PAGE, len: 0x1000, object: NONE, ..SpawnMap::default() })), laid.entry, invalid);
    refused("two ranges that meet", with(&|m| m.push(m[0])), laid.entry, invalid);
    refused("an entry in no executable range", honest.clone(), honest[0].addr + honest[0].len - 8, invalid);

    // A handle without MAP is no right to the object's bytes.
    let object = honest[text].object as u32;
    // SAFETY: nothing relies on being the source's only holder.
    let unmappable = unsafe { syscall::dup_narrowed(RawHandle(object), Rights::DUP.union(Rights::TRANSFER)) }.expect("a duplicate without MAP");
    refused("an object without MAP", with(&|m| m[text].object = u64::from(unmappable.0)), laid.entry, SyscallError::PermissionDenied);
    // SAFETY: no type holds this number.
    unsafe { syscall::close(unmappable) };

    runs("after every refusal", &image::image_of(SELF));
    println!("abuse_spawn_maps: PASS");
}
