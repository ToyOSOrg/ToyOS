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

use toyos_abi::end::{End, Fault};
use toyos_abi::RawHandle;

use crate::errcode::EBADF;

const O_CLOEXEC: i32 = 0x80000;

/// The child's descriptors are its slots, each at its first generation.
pub(crate) const CHILD_FDS: usize = RawHandle::MAX_SLOTS;

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
    u32::try_from(fd).ok().filter(|&fd| (fd as usize) < CHILD_FDS).ok_or(EBADF)
}

/// A descriptor an action reads or closes, `adddup2`'s `fd` and
/// `addclose`'s: one of the child's, or of this process's, whose numbers run
/// past the child's slots once a slot is reused (`RawHandle`'s generation).
pub(crate) fn any_fd(fd: i32) -> Result<u32, i32> {
    u32::try_from(fd).map_err(|_| EBADF)
}

/// What the spawn hands the kernel: `[child slot, this process's handle]`
/// for every descriptor the child starts with, and the handles the actions
/// opened here, which are this process's to close once the spawn returns.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Plan {
    pub(crate) slots: Vec<[u32; 2]>,
    pub(crate) opened: Vec<u32>,
}

/// One of the child's descriptors while the actions run: the handle of this
/// process's it is, and whether the exec closes it.
#[derive(Clone, Copy)]
struct Entry {
    handle: u32,
    cloexec: bool,
}

/// The child's table: `stdio[n]` is this process's handle for descriptor `n`
/// and whether it is marked close-on-exec, `None` for one it does not hold;
/// then each action in order, as POSIX runs them in the child; then the exec,
/// which closes what is marked. A descriptor of this process's beyond 2 that
/// no action closed is one an action may name, and is not the child's
/// otherwise. `open` opens a path here, answering the handle or an `errno`;
/// a refusal ends the plan with the handles it opened closed by `close`.
pub(crate) fn plan<P>(
    stdio: [Option<(u32, bool)>; 3],
    actions: &[Action<P>],
    mut open: impl FnMut(&P, i32, u32) -> Result<u32, i32>,
    mut close: impl FnMut(u32),
) -> Result<Plan, i32> {
    let mut table: BTreeMap<u32, Entry> = BTreeMap::new();
    for (fd, held) in (0..).zip(stdio) {
        if let Some((handle, cloexec)) = held {
            table.insert(fd, Entry { handle, cloexec });
        }
    }
    // Descriptors this process holds beyond 2 that an action closed in the child.
    let mut closed: BTreeSet<u32> = BTreeSet::new();
    let mut opened = Vec::new();
    let refuse = |opened: &mut Vec<u32>, close: &mut dyn FnMut(u32), errno| {
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
                    Some(entry) => entry.handle,
                    None if *fd > 2 && !closed.contains(fd) => *fd,
                    None => return refuse(&mut opened, &mut close, EBADF),
                };
                table.insert(*newfd, Entry { handle: source, cloexec: false });
                closed.remove(newfd);
            }
        }
    }
    let slots = table.into_iter().filter(|(_, e)| !e.cloexec).map(|(fd, e)| [fd, e.handle]).collect();
    Ok(Plan { slots, opened })
}

/// The status `waitpid` writes for an end, as `<sys/wait.h>`'s macros read
/// it: an exit's low byte above the low eight bits, or a signal's number in
/// the low seven. A kill is `SIGKILL`; a fault is its `<signal.h>` class's
/// signal, and a handle fault `SIGSEGV`'s, as a pointer is the other way to
/// name what a process does not hold.
pub(crate) fn wait_status(end: End) -> i32 {
    const SIGILL: i32 = 4;
    const SIGTRAP: i32 = 5;
    const SIGBUS: i32 = 7;
    const SIGFPE: i32 = 8;
    const SIGKILL: i32 = 9;
    const SIGSEGV: i32 = 11;
    match end {
        End::Exited(code) => (code & 0xff) << 8,
        End::Killed => SIGKILL,
        End::Fault(Fault::Memory | Fault::Handle) => SIGSEGV,
        End::Fault(Fault::Alignment) => SIGBUS,
        End::Fault(Fault::Instruction) => SIGILL,
        End::Fault(Fault::Arithmetic) => SIGFPE,
        End::Fault(Fault::Trap) => SIGTRAP,
    }
}
