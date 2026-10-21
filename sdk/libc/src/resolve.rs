//! Where a path leads, as `realpath` answers it: by the kernel's rules of
//! resolution (`kernel/src/vfs.rs`, `resolve_absolute` and
//! `resolve_for_open`), which every path libc hands the kernel is read by. It
//! asks the kernel only through [`Kernel`], so the host tests it
//! (`toyos-libc-copies`).
//!
//! A path a file server holds is resolved POSIX's way by [`resolve_path`],
//! which hands the kernel what it holds.
//!
//! The kernel's rules are not POSIX's (`issues/the-kernel-resolves-a-path-by-rules-of-its-own.md`):
//! `..` is read off the text before any name is looked up, only the last
//! name's link is followed, at most [`MAX_LINKS`] of them, and a relative link
//! is read against the mount it is on, the first name of its path. The kernel
//! follows no other name as a link, so a path through one names nothing, and
//! an answer holds none.

use alloc::vec::Vec;

use toyos_abi::syscall::{SyscallError, MAX_LINKS, MAX_PATH};

use crate::errcode::{self, ELOOP, ENAMETOOLONG, ENOENT, ENOTDIR};

/// The three calls resolution asks the kernel, each of an absolute path.
pub(crate) trait Kernel {
    /// `SYS_READLINK` into `target`, [`MAX_PATH`] bytes: the length it wrote.
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
    let mut followed = 0;
    let mut target = alloc::vec![0u8; MAX_PATH];
    loop {
        let len = match kernel.read_link(&at, &mut target) {
            Ok(len) => len,
            // A name that is no link, or none at all: `kind` tells them apart.
            Err(SyscallError::NotFound) => break,
            Err(e) => return Err(errcode::of(e)),
        };
        if followed == MAX_LINKS {
            return Err(ELOOP);
        }
        followed += 1;
        let mount = at.split(|&b| b == b'/').nth(1).expect("an absolute path's first name");
        let mut base = Vec::with_capacity(1 + mount.len());
        base.push(b'/');
        base.extend_from_slice(mount);
        at = joined(&base, &target[..len])?;
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
/// or `ENAMETOOLONG` where it or the answer reaches [`MAX_PATH`]: a link
/// target that fills the buffer may have been cut short.
fn joined(base: &[u8], rel: &[u8]) -> Result<Vec<u8>, i32> {
    if rel.len() >= MAX_PATH {
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
    if out.len() >= MAX_PATH {
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

/// What a name a file server holds is, as resolution asks it, its own link
/// not followed.
pub(crate) enum Name {
    Link(Vec<u8>),
    Other,
}

/// The two calls [`resolve_path`] asks of the file servers, each of an
/// absolute path without `.` or `..`.
pub(crate) trait Served {
    /// Whether a file server holds `at`, and not the kernel.
    fn holds(&mut self, at: &[u8]) -> bool;
    /// `at` itself, or the errno it is refused with: one that names nothing,
    /// or passes through a file, is refused.
    fn name(&mut self, at: &[u8]) -> Result<Name, i32>;
}

/// `path` made absolute against `cwd` and resolved as POSIX resolves it, a
/// name at a time, while a file server holds the names: each link is
/// followed where it is met, a relative one against its own directory, and
/// `..` after it is the parent of where it led. At the first name the kernel
/// holds, the rest is [`resolve`]d by the kernel's rules, which its `open`
/// reads it by. The answer holds no link a file server keeps.
pub(crate) fn resolve_path(cwd: &[u8], path: &[u8], served: &mut impl Served, kernel: &mut impl Kernel) -> Result<Vec<u8>, i32> {
    if path.is_empty() {
        return Err(ENOENT);
    }
    let names = |text: &[u8]| -> Vec<Vec<u8>> { text.split(|&b| b == b'/').map(<[u8]>::to_vec).collect() };
    let mut pending: alloc::collections::VecDeque<Vec<u8>> = alloc::collections::VecDeque::new();
    if path.first() != Some(&b'/') {
        pending.extend(names(cwd));
    }
    pending.extend(names(path));
    let mut at: Vec<u8> = Vec::new();
    let mut followed = 0;
    while let Some(name) = pending.pop_front() {
        match &name[..] {
            b"" | b"." => continue,
            b".." => {
                let parent = at.iter().rposition(|&b| b == b'/').unwrap_or(0);
                at.truncate(parent);
                continue;
            }
            _ => {}
        }
        let mut next = at.clone();
        next.push(b'/');
        next.extend_from_slice(&name);
        if next.len() >= MAX_PATH {
            return Err(ENAMETOOLONG);
        }
        if !served.holds(&next) {
            for rest in pending {
                next.push(b'/');
                next.extend_from_slice(&rest);
            }
            return resolve(b"/", &next, kernel);
        }
        match served.name(&next)? {
            Name::Other => at = next,
            Name::Link(target) => {
                if followed == MAX_LINKS {
                    return Err(ELOOP);
                }
                followed += 1;
                if target.first() == Some(&b'/') {
                    at.clear();
                }
                for name in names(&target).into_iter().rev() {
                    pending.push_front(name);
                }
            }
        }
    }
    if at.is_empty() {
        at.push(b'/');
    }
    Ok(at)
}
