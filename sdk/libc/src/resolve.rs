//! POSIX canonical path resolution through the process's filesystem namespace.
use alloc::vec::Vec;
use toyos_abi::syscall::MAX_PATH;
use toyos::files::MAX_LINKS;
use crate::errcode::{ELOOP, ENAMETOOLONG, ENOENT, ENOTDIR};

/// What a name a file server holds is, as resolution asks it, its own link
/// not followed.
pub(crate) enum Name {
    Link(Vec<u8>),
    Directory,
    Other,
}

/// The two calls [`resolve_path`] asks of the file servers, each of an
/// absolute path without `.` or `..`.
pub(crate) trait Served {
    /// `at` itself, or the errno it is refused with: one that names nothing,
    /// or passes through a file, is refused.
    fn name(&mut self, at: &[u8]) -> Result<Name, i32>;
}

/// Follow each name's links where encountered, including before `..`.
pub(crate) fn resolve_path(cwd: &[u8], path: &[u8], served: &mut impl Served) -> Result<Vec<u8>, i32> {
    if path.is_empty() {
        return Err(ENOENT);
    }
    if path.len() >= MAX_PATH { return Err(ENAMETOOLONG); }
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
        match served.name(&next)? {
            Name::Directory => at = next,
            Name::Other if pending.is_empty() => at = next,
            Name::Other => return Err(ENOTDIR),
            Name::Link(target) => {
                if target.len() >= MAX_PATH { return Err(ENAMETOOLONG); }
                if target.is_empty() { return Err(ENOENT); }
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
