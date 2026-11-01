//! What `posix_spawn` decides before the kernel is asked: the child's
//! descriptors, from this process's stdin, stdout and stderr and the file
//! actions; and what `waitpid` answers for a child's end. It reads nothing but
//! what it is handed, so the host tests it (`toyos-libc-copies`).
//!
//! **A child starts with descriptors 0–2 and exactly what its file actions
//! name**, a stated departure from POSIX, whose child inherits every
//! descriptor not marked close-on-exec: this process's other descriptors are
//! not the child's to hold unless an action names one.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;

use toyos_abi::end::End;

use crate::errcode::EBADF;

const O_CLOEXEC: i32 = 0x80000;

/// One `posix_spawn_file_actions_add*`, in the child's descriptor numbers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Action<P> {
    Open { fd: u32, path: P, oflag: i32, mode: u32 },
    Close { fd: u32 },
    Dup2 { fd: u32, newfd: u32 },
}

/// A descriptor of the child's an action makes, `addopen`'s and `adddup2`'s
/// `newfd`, or `EBADF` for one no child's table has.
pub(crate) fn child_fd(fd: i32) -> Result<u32, i32> {
    u32::try_from(fd).ok().filter(|&fd| (fd as usize) < toyos::fd::MAX_FDS).ok_or(EBADF)
}

/// A descriptor an action reads or closes, `adddup2`'s `fd` and
/// `addclose`'s: one of the child's, or of this process's.
pub(crate) fn any_fd(fd: i32) -> Result<u32, i32> {
    u32::try_from(fd).ok().filter(|&fd| (fd as usize) < toyos::fd::MAX_FDS).ok_or(EBADF)
}

/// What the spawn gives the child: `(child descriptor, what of this
/// process's it is)` for every descriptor it starts with, and the handles the
/// actions opened here, which are this process's to close once the spawn
/// returns.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Plan<H> {
    pub(crate) fds: Vec<(u32, H)>,
    pub(crate) opened: Vec<H>,
}

/// One of the child's descriptors while the actions run: what of this
/// process's it is, and whether the exec closes it.
#[derive(Clone, Copy)]
struct Entry<H> {
    handle: H,
    cloexec: bool,
}

/// The child's table: `held(n)` is this process's handle for its descriptor
/// `n` and whether it is marked close-on-exec, `None` for one it does not
/// hold, or the `errno` of one it holds but cannot give; 0 to 2 first, then each action in order, as POSIX runs them in the
/// child; then the exec, which closes what is marked. A descriptor of this
/// process's beyond 2 that no action closed is one an action may name, and is
/// not the child's otherwise. `open` opens a path here, answering the handle
/// or an `errno`; a refusal ends the plan with the handles it opened closed
/// by `close`.
pub(crate) fn plan<P, H: Copy>(
    mut held: impl FnMut(u32) -> Result<Option<(H, bool)>, i32>,
    actions: &[Action<P>],
    mut open: impl FnMut(&P, i32, u32) -> Result<H, i32>,
    mut close: impl FnMut(H),
) -> Result<Plan<H>, i32> {
    let mut table: BTreeMap<u32, Entry<H>> = BTreeMap::new();
    for fd in 0..3 {
        if let Some((handle, cloexec)) = held(fd)? {
            table.insert(fd, Entry { handle, cloexec });
        }
    }
    // Descriptors this process holds beyond 2 that an action closed in the child.
    let mut closed: BTreeSet<u32> = BTreeSet::new();
    let mut opened = Vec::new();
    let refuse = |opened: &mut Vec<H>, close: &mut dyn FnMut(H), errno| {
        opened.drain(..).for_each(close);
        Err(errno)
    };
    for action in actions {
        match action {
            Action::Open { fd, path, oflag, mode } => match open(path, *oflag, *mode) {
                Ok(handle) => {
                    opened.push(handle);
                    table.insert(*fd, Entry { handle, cloexec: oflag & O_CLOEXEC != 0 });
                }
                Err(errno) => return refuse(&mut opened, &mut close, errno),
            },
            Action::Close { fd } => {
                table.remove(fd);
                if *fd > 2 {
                    closed.insert(*fd);
                }
            }
            Action::Dup2 { fd, newfd } => {
                let source = match table.get(fd) {
                    Some(entry) => Ok(Some(entry.handle)),
                    None if *fd > 2 && !closed.contains(fd) => held(*fd).map(|held| held.map(|(handle, _)| handle)),
                    None => Ok(None),
                };
                let source = match source {
                    Ok(Some(source)) => source,
                    Ok(None) => return refuse(&mut opened, &mut close, EBADF),
                    Err(errno) => return refuse(&mut opened, &mut close, errno),
                };
                table.insert(*newfd, Entry { handle: source, cloexec: false });
                closed.remove(newfd);
            }
        }
    }
    let fds = table.into_iter().filter(|(_, e)| !e.cloexec).map(|(fd, e)| (fd, e.handle)).collect();
    Ok(Plan { fds, opened })
}

/// What one turn of a wait does with the children it names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Turn {
    /// None is left to wait for: `ECHILD`.
    Gone,
    /// The named child at this index has ended and is answered.
    Take(usize),
    /// Every named child left is running.
    Watch,
}

/// The turn for the named children whose ends `ended` holds, each read once.
/// **With `SIGCHLD` ignored an ended child is forgotten, never answered**: the
/// caller drops every child this read saw ended, and a child that ends after
/// the read is the next turn's.
pub(crate) fn turn(ended: &[bool], ignored: bool) -> Turn {
    if ignored {
        return if ended.contains(&false) { Turn::Watch } else { Turn::Gone };
    }
    match ended.iter().position(|&e| e) {
        Some(at) => Turn::Take(at),
        None if ended.is_empty() => Turn::Gone,
        None => Turn::Watch,
    }
}

/// The status `waitpid` writes for an end, as `<sys/wait.h>`'s macros read
/// it: an exit's low byte above the low eight bits, or the signal
/// `toyos::process::signal` names the end by in the low seven.
pub(crate) fn wait_status(end: End) -> i32 {
    match (end, toyos::process::signal(end)) {
        (End::Exited(code), _) => (code & 0xff) << 8,
        (_, Some(signal)) => signal,
        (_, None) => unreachable!("only an exit names no signal"),
    }
}
