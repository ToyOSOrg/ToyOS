//! The filesystem calls: a path in, resolved against the caller's cwd, and the
//! volume read or changed.
//!
//! Every path arrives as a `&str` `super::dispatch` already copied out of user
//! memory — a `&str` over a page userland can rewrite is a path that is one
//! thing when it is resolved and another when it is opened, so no arm here ever
//! holds a user pointer.
//!
//! What varies between these calls is one question, and [`resolve_and_check`]
//! is where it is asked: may this caller modify what this path names?
//! [`sys_open`] asks it only for a modifying open; every mutating call asks it
//! always. The check rides the same `vfs` guard the mutation will run on, which
//! is what keeps the answer true at the moment it acts.

use crate::object::ops;
use crate::user_ptr::UserBytesMut;
use crate::{log, process, vfs};

use toyos_abi::syscall::*;

/// Whether `flags` ask for anything that can change what is on the volume.
///
/// `WRITE` alone is not the question: `CREATE` makes a file, `TRUNCATE`
/// destroys one's contents, and `APPEND` is a write position. A read-only open
/// of a `KernelOnly` mount is fine and stays fine — the handle it hands back has
/// `writable` false, so nothing downstream needs a second check.
fn open_modifies(flags: OpenFlags) -> bool {
    flags.contains(OpenFlags::WRITE)
        || flags.contains(OpenFlags::CREATE)
        || flags.contains(OpenFlags::TRUNCATE)
        || flags.contains(OpenFlags::APPEND)
}

/// Resolve `path` against `cwd` on `vfs` and — when `demand` — require the
/// caller may modify what it names, refusing with `PermissionDenied` otherwise.
///
/// The prologue every write-side filesystem syscall shares, resolve and check on
/// one guard. **The check rides the guard the mutation will run on**, so nothing
/// about the mount table can shift between deciding and acting — a resolve on one
/// `vfs::lock()` and a `user_may_modify` on a second could disagree if a mount
/// moved between them. Nothing moves one after boot regardless (`Vfs::mount`
/// runs only from `main.rs`; no mount syscall exists), so the single guard is a
/// structural guarantee rather than a fix for a live race. `demand` is the whole
/// of what varies: `sys_open` demands only for a modifying open, every other
/// caller always.
fn resolve_and_check(
    vfs: &vfs::Vfs,
    cwd: &str,
    path: &str,
    demand: bool,
) -> Result<alloc::string::String, u64> {
    let resolved = vfs.resolve_absolute(cwd, path);
    if demand && !vfs.user_may_modify(&resolved) {
        return Err(SyscallError::PermissionDenied.to_u64());
    }
    Ok(resolved)
}

/// Clone the cwd, take the vfs lock, and [`resolve_and_check`] one path with the
/// modify demand on — the prologue every single-path mutating syscall shares.
///
/// The guard comes back held with the resolved path, so the mutation runs under
/// the same lock the check was made on. `sys_open` (a conditional demand) and
/// `sys_rename` (two paths under one guard) call [`resolve_and_check`] directly
/// instead, for the parts of the shape they do not fit.
fn resolve_for_modify(path: &str) -> Result<(vfs::VfsGuard, alloc::string::String), u64> {
    let cwd = process::with_process_data(|d| d.cwd.clone());
    let vfs = vfs::lock();
    let resolved = resolve_and_check(&vfs, &cwd, path, true)?;
    Ok((vfs, resolved))
}

pub(super) fn sys_open(path: &str, flags: OpenFlags) -> u64 {
    let cwd = process::with_process_data(|d| d.cwd.clone());
    let resolved = {
        let vfs = vfs::lock();
        match resolve_and_check(&vfs, &cwd, path, open_modifies(flags)) {
            Ok(resolved) => resolved,
            Err(refusal) => return refusal,
        }
    };
    process::with_process_data(|data| ops::open(&mut data.handles, &resolved, flags))
}

/// Encode a directory listing into `buf`; return the length it *needs*.
///
/// Same contract as `sys_getcwd`, for the same reason and after the same
/// defect: this used to fill the buffer, stop, and report the bytes it had
/// written, which is indistinguishable from a complete listing. Measured
/// before the change: `std::fs::read_dir` reported **4125** entries of
/// **34,816**, as success. A caller enumerating a directory to delete it, or
/// to check a name is absent, acts on that.
///
/// So the listing is written only when all of it fits, and the return is the
/// size either way: `n <= buf.len()` means the entries are in the buffer,
/// `n > buf.len()` means nothing was written and `n` is what to allocate.
/// Refusing to write a partial answer is the point — a caller that ignores
/// the return still gets zeroes rather than a plausible short listing.
pub(super) fn sys_readdir(path: &str, out: &mut UserBytesMut) -> u64 {
    let cwd = process::with_process_data(|d| d.cwd.clone());
    let entries = match vfs::lock().list(&cwd, path) {
        Ok(e) => e,
        Err(e) => return e.to_u64(),
    };

    // A directory name is stored with its trailing slash and encoded without.
    let encoded = |name: &alloc::string::String| 1 + name.trim_end_matches('/').len() + 1 + 8;
    let needed: usize = entries.iter().map(|(name, _)| encoded(name)).sum();
    if needed > out.len() {
        return needed as u64;
    }

    let mut pos = 0;
    for (name, size) in &entries {
        let is_dir = name.ends_with('/');
        let clean_name = if is_dir { &name[..name.len() - 1] } else { name.as_str() };
        out.write_at(pos, &[if is_dir { 2 } else { 1 }]);
        pos += 1;
        out.write_at(pos, clean_name.as_bytes());
        pos += clean_name.len();
        out.write_at(pos, &[0]);
        pos += 1;
        out.write_at(pos, &size.to_le_bytes());
        pos += 8;
    }
    debug_assert_eq!(pos, needed);
    pos as u64
}

pub(super) fn sys_delete(path: &str) -> u64 {
    let (mut vfs, resolved) = match resolve_for_modify(path) {
        Ok(pair) => pair,
        Err(refusal) => return refusal,
    };
    match vfs.delete(&resolved) {
        Ok(()) => 0,
        Err(e) => e.to_u64(),
    }
}

pub(super) fn sys_chdir(path: &str) -> u64 {
    let cwd = process::with_process_data(|d| d.cwd.clone());
    match vfs::lock().cd(&cwd, path) {
        Ok(new_cwd) => {
            process::with_process_data(|d| d.cwd = new_cwd);
            0
        }
        Err(e) => e.to_u64(),
    }
}

/// Copy the cwd into `buf`; return the length the cwd *needs*.
///
/// The return is the required length, not the number of bytes written, so a
/// caller compares it against the buffer it passed: `n <= buf.len()` means the
/// path is in the buffer, `n > buf.len()` means nothing was written and `n` is
/// the size to allocate before retrying.
///
/// That distinction is the whole point. The old contract returned
/// `min(cwd.len(), buf.len())` and wrote a prefix, so "fit exactly" and
/// "silently truncated" were the same answer — and `std::env::current_dir`,
/// which passes a fixed 256-byte buffer, handed back a *different, valid-
/// looking* path for any longer cwd. A wrong answer that looks right is worse
/// than an error: it propagates into every path the program derives from it.
///
/// Nothing is written when the buffer is too small. A partial path names the
/// wrong directory, and leaving one in the caller's buffer invites its use.
///
/// An empty buffer is therefore a size query, which falls out rather than
/// being bolted on: the dispatch hands `user_bytes_mut` a zero length back as
/// an empty window, so `getcwd(NULL, 0)` reports the length and touches nothing.
///
/// `vfs::MAX_PATH` bounds `cwd`, so the required length is always far below the
/// range `SyscallError` encodes and can never be misread as one.
pub(super) fn sys_getcwd(out: &mut UserBytesMut) -> u64 {
    process::with_process_data(|data| {
        let cwd = data.cwd.as_bytes();
        if cwd.len() <= out.len() {
            out.write_at(0, cwd);
        }
        cwd.len() as u64
    })
}

pub(super) fn sys_rename(old: &str, new: &str) -> u64 {
    let cwd = process::with_process_data(|d| d.cwd.clone());
    let mut vfs = vfs::lock();
    let old_abs = match resolve_and_check(&vfs, &cwd, old, true) {
        Ok(resolved) => resolved,
        Err(refusal) => return refusal,
    };
    let new_abs = match resolve_and_check(&vfs, &cwd, new, true) {
        Ok(resolved) => resolved,
        Err(refusal) => return refusal,
    };
    match vfs.rename(&old_abs, &new_abs) {
        Ok(()) => 0,
        Err(e) => e.to_u64(),
    }
}

pub(super) fn sys_mkdir(path: &str) -> u64 {
    let (mut vfs, resolved) = match resolve_for_modify(path) {
        Ok(pair) => pair,
        Err(refusal) => return refusal,
    };
    match vfs.create_dir(&resolved) {
        Ok(()) => 0,
        Err(e) => e.to_u64(),
    }
}

pub(super) fn sys_rmdir(path: &str) -> u64 {
    let (mut vfs, resolved) = match resolve_for_modify(path) {
        Ok(pair) => pair,
        Err(refusal) => return refusal,
    };
    vfs.remove_dir(&resolved);
    0
}

pub(super) fn sys_symlink(target: &str, link: &str) -> u64 {
    let (mut vfs, resolved) = match resolve_for_modify(link) {
        Ok(pair) => pair,
        Err(refusal) => return refusal,
    };
    match vfs.create_symlink(&resolved, target) {
        Ok(()) => 0,
        Err(e) => {
            log!("symlink({target} -> {link}): {e}");
            e.to_u64()
        }
    }
}

pub(super) fn sys_readlink(path: &str, out: &mut UserBytesMut) -> u64 {
    let cwd = process::with_process_data(|d| d.cwd.clone());
    let mut vfs = vfs::lock();
    let resolved = vfs.resolve_absolute(&cwd, path);
    match vfs.read_link(&resolved) {
        Ok(Some(target)) => {
            let bytes = target.as_bytes();
            let len = bytes.len().min(out.len());
            out.write_at(0, &bytes[..len]);
            len as u64
        }
        Ok(None) => SyscallError::NotFound.to_u64(),
        Err(e) => e.to_u64(),
    }
}
