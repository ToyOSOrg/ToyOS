//! Where a path leads, as `realpath` answers it: by the kernel's rules of
//! resolution (`kernel/src/vfs.rs`, `resolve_absolute` and
//! `resolve_for_open`), which every path libc hands the kernel is read by. It
//! asks the kernel only through [`Kernel`], so the host tests it
//! (`toyos-libc-copies`).
//!
//! The rules are not POSIX's: `..` is read off the text before any name is
//! looked up, only the last name's link is followed, and a relative link is
//! read against the mount it is on, the first name of its path. The kernel
//! follows no other name as a link, so a path through one names nothing, and
//! an answer holds none.

use alloc::vec::Vec;

use toyos_abi::syscall::SyscallError;

use crate::errcode::{self, ELOOP, ENAMETOOLONG, ENOENT, ENOTDIR};

/// `limits.h`'s `PATH_MAX`: the longest path, counting its NUL.
pub(crate) const PATH_MAX: usize = 4096;

/// The three calls resolution asks the kernel, each of an absolute path.
pub(crate) trait Kernel {
    /// `SYS_READLINK` into `target`, [`PATH_MAX`] bytes: the length it wrote.
    fn read_link(&mut self, at: &[u8], target: &mut [u8]) -> Result<usize, SyscallError>;
    /// Whether `SYS_OPEN` opens `at` to read; what it opened is closed.
    fn open(&mut self, at: &[u8]) -> Result<(), SyscallError>;
    /// Whether `SYS_READDIR` lists `at`.
    fn is_dir(&mut self, at: &[u8]) -> bool;
}

/// `path` made absolute against `cwd`, its last name's links followed, or the
/// errno it is refused with.
pub(crate) fn resolve(cwd: &[u8], path: &[u8], kernel: &mut impl Kernel) -> Result<Vec<u8>, i32> {
    if path.is_empty() {
        return Err(ENOENT);
    }
    let mut at = joined(cwd, path)?;
    let mut followed: Vec<Vec<u8>> = Vec::new();
    let mut target = alloc::vec![0u8; PATH_MAX];
    loop {
        let len = match kernel.read_link(&at, &mut target) {
            Ok(len) => len,
            // A name that is no link, or none at all: `kind` tells them apart.
            Err(SyscallError::NotFound) => break,
            Err(e) => return Err(errcode::of(e)),
        };
        // A cycle, not a count: any chain without one ends in a name.
        if followed.contains(&at) {
            return Err(ELOOP);
        }
        let mount = at.split(|&b| b == b'/').nth(1).expect("an absolute path's first name");
        let mut base = Vec::with_capacity(1 + mount.len());
        base.push(b'/');
        base.extend_from_slice(mount);
        let next = joined(&base, &target[..len])?;
        followed.push(core::mem::replace(&mut at, next));
    }
    match kind(&at, kernel)? {
        Kind::File | Kind::Dir => Ok(at),
        Kind::Absent => Err(absent(&at, kernel)?),
    }
}

/// What a path no link is followed in names.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    File,
    Dir,
    Absent,
}

/// `open` refuses every directory, so a refused name is asked `readdir`.
fn kind(at: &[u8], kernel: &mut impl Kernel) -> Result<Kind, i32> {
    match kernel.open(at) {
        Ok(()) => Ok(Kind::File),
        Err(_) if kernel.is_dir(at) => Ok(Kind::Dir),
        Err(SyscallError::NotFound) => Ok(Kind::Absent),
        Err(e) => Err(errcode::of(e)),
    }
}

/// `rel` read against the absolute `base`, `.` and `..` taken off the text,
/// or `ENAMETOOLONG` where it or the answer reaches [`PATH_MAX`]: a link
/// target that fills the buffer may have been cut short.
fn joined(base: &[u8], rel: &[u8]) -> Result<Vec<u8>, i32> {
    if rel.len() >= PATH_MAX {
        return Err(ENAMETOOLONG);
    }
    let text = if rel.first() == Some(&b'/') { &[][..] } else { base };
    let mut names: Vec<&[u8]> = Vec::new();
    for name in text.split(|&b| b == b'/').chain(rel.split(|&b| b == b'/')) {
        match name {
            b"" | b"." => {}
            b".." => {
                names.pop();
            }
            name => names.push(name),
        }
    }
    let mut out = Vec::new();
    for name in &names {
        out.push(b'/');
        out.extend_from_slice(name);
    }
    if out.is_empty() {
        out.push(b'/');
    }
    if out.len() >= PATH_MAX {
        return Err(ENAMETOOLONG);
    }
    Ok(out)
}

/// Why `at`, which names nothing, does: `ENOTDIR` where a directory it passes
/// through is a file, `ENOENT` otherwise.
fn absent(at: &[u8], kernel: &mut impl Kernel) -> Result<i32, i32> {
    for end in (1..at.len()).filter(|&i| at[i] == b'/') {
        match kind(&at[..end], kernel)? {
            Kind::Dir => {}
            Kind::File => return Ok(ENOTDIR),
            Kind::Absent => return Ok(ENOENT),
        }
    }
    Ok(ENOENT)
}
