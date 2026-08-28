//! Filesystem syscalls: resolve a path against the caller's cwd, then read or
//! change the volume.
//!
//! Every path is a `&str` `super::dispatch` already copied out of user memory,
//! so no arm here holds a user pointer.

use crate::object::ops;
use crate::user_ptr::UserBytesMut;
use crate::{log, process, vfs};

use toyos_abi::syscall::*;

// Entry ops act on the name without following it; the check rides the same `vfs`
// guard the mutation will run on, so a moved mount can't separate check from act.
fn resolve_and_check(
    vfs: &vfs::Vfs,
    cwd: &str,
    path: &str,
) -> Result<alloc::string::String, u64> {
    let resolved = vfs.resolve_absolute(cwd, path);
    if !vfs.user_may_modify(&resolved) {
        return Err(SyscallError::PermissionDenied.to_u64());
    }
    Ok(resolved)
}

// Returns the guard held, so the mutation runs under the same lock the check ran on.
fn resolve_for_modify(path: &str) -> Result<(vfs::VfsGuard, alloc::string::String), u64> {
    let cwd = process::with_process_data(|d| d.cwd.clone());
    let vfs = vfs::lock();
    let resolved = resolve_and_check(&vfs, &cwd, path)?;
    Ok((vfs, resolved))
}

pub(super) fn sys_open(path: &str, flags: OpenFlags) -> u64 {
    let cwd = process::with_process_data(|d| d.cwd.clone());
    let resolved = vfs::lock().resolve_absolute(&cwd, path);
    process::with_process_data(|data| ops::open(&mut data.handles, &resolved, flags))
}

/// Returns the length needed; writes the listing only when it fits (`n <= out.len()`).
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

/// Returns the length needed; writes the cwd only when it fits (`n <= out.len()`).
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
    let old_abs = match resolve_and_check(&vfs, &cwd, old) {
        Ok(resolved) => resolved,
        Err(refusal) => return refusal,
    };
    let new_abs = match resolve_and_check(&vfs, &cwd, new) {
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
