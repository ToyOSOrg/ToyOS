//! `/` is a synthesized read-only directory, and `/apps` and `/home` are two
//! paths into one filesystem.
//!
//! The listing of `/` is a closed set, so a stray top-level name cannot appear
//! because some volume happens to carry a directory by that name; `/` is no
//! filesystem either, so every syscall that would change what is at it is
//! refused the way a read-only mount refuses one, and the machine survives it.
//!
//! The two files this writes are the other half of
//! `apps_and_home_are_one_filesystem` in `tests/common/storage.rs`.

use std::fs;
use std::io::Write;

use toyos_abi::syscall::{self, OpenFlags, SyscallError};

/// `vfs::ROOT_ENTRIES`: the kernel's set and this one are two spellings of the
/// hierarchy, so a boot that grew an eighth entry reds here.
const ROOT_ENTRIES: [&str; 7] = ["apps", "boot", "home", "log", "media", "system", "tmp"];

/// Mirrored in `tests/common/storage.rs`, whose reader sees them without the
/// mount point, inside the one volume.
const IN_HOME: &str = "/home/hierarchy-home.bin";
const IN_APPS: &str = "/apps/hierarchy-apps.bin";
const LEN: usize = 2 * 4096 + 61;

fn payload(seed: u8) -> Vec<u8> {
    (0..LEN).map(|i| (i.wrapping_mul(53) ^ seed as usize) as u8).collect()
}

fn names(dir: &str) -> Vec<String> {
    let mut out: Vec<String> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read_dir {dir}: {e}"))
        .map(|e| e.expect("dir entry").file_name().to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
}

fn main() {
    let listed = names("/");
    assert_eq!(listed, ROOT_ENTRIES, "/ lists {listed:?}");
    println!("  PASS / lists exactly {:?}", ROOT_ENTRIES);

    root_refuses_every_way_of_changing_it();
    media_is_an_empty_directory();
    apps_and_home_take_writes();

    println!("all hierarchy tests passed");
}

/// Every syscall that can change what is at a path, aimed at `/`. One each
/// rather than one representative: they are separate entry points, so a gate on
/// `open` alone says nothing about `unlink`. None may end the machine.
fn root_refuses_every_way_of_changing_it() {
    assert_eq!(
        syscall::open(b"/intruder", OpenFlags::CREATE | OpenFlags::WRITE),
        Err(SyscallError::PermissionDenied),
        "creating a file at / was permitted"
    );
    assert_eq!(
        syscall::delete(b"/system"),
        Err(SyscallError::PermissionDenied),
        "unlinking a mount point was permitted"
    );
    assert_eq!(
        syscall::rename(b"/system", b"/elsewhere"),
        Err(SyscallError::PermissionDenied),
        "renaming a mount point was permitted"
    );
    assert_eq!(
        syscall::mkdir(b"/intruder"),
        Err(SyscallError::PermissionDenied),
        "mkdir at / was permitted"
    );
    // `InvalidArgument` and not `PermissionDenied`: a mount point is not a
    // directory a caller may remove even where the mount itself is writable.
    assert_eq!(
        syscall::rmdir(b"/tmp"),
        Err(SyscallError::InvalidArgument),
        "rmdir of a mount point was permitted"
    );
    assert_eq!(
        syscall::symlink(b"/system/bin/init", b"/intruder"),
        Err(SyscallError::PermissionDenied),
        "a symlink at / was permitted"
    );

    // The machine is still here, and / still holds what it held.
    let after = names("/");
    assert_eq!(after, ROOT_ENTRIES, "a refused operation changed / to {after:?}");
    assert!(fs::metadata("/system/bin/init").is_ok(), "/system is unreadable after the refusals");
    println!("  PASS create, unlink, rename, mkdir, rmdir and symlink are all refused at /");
}

/// A bare mount point is an empty directory, not an error: foreign volumes land
/// under it at the track's stage 5.
fn media_is_an_empty_directory() {
    let listed = names("/media");
    assert!(listed.is_empty(), "/media holds {listed:?}");
    assert!(fs::read("/media/anything").is_err(), "/media served a file");
    println!("  PASS /media is an empty directory");
}

/// One file under each name DATA answers to, fsynced, so the host's reader finds
/// them on the device rather than in a cache.
fn apps_and_home_take_writes() {
    for (path, seed) in [(IN_HOME, 0xA5u8), (IN_APPS, 0x5A)] {
        let data = payload(seed);
        let mut f = fs::File::create(path).unwrap_or_else(|e| panic!("create {path}: {e}"));
        f.write_all(&data).unwrap_or_else(|e| panic!("write {path}: {e}"));
        f.sync_all().unwrap_or_else(|e| panic!("fsync {path}: {e}"));
        drop(f);
        let back = fs::read(path).unwrap_or_else(|e| panic!("read back {path}: {e}"));
        assert_eq!(back, data, "{path} did not read back as it was written");
    }
    // Two paths, not two names for one file: a symlink between them would break this.
    assert!(fs::read("/apps/hierarchy-home.bin").is_err(), "/apps answered for a /home file");
    assert!(fs::read("/home/hierarchy-apps.bin").is_err(), "/home answered for an /apps file");
    println!("  PASS {LEN} bytes under each of /home and /apps, and neither is the other");
}
