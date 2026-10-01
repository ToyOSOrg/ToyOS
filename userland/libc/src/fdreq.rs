//! What `fcntl` decides before the kernel is asked, and the close-on-exec
//! marks `open`, `fcntl`, `close` and `dup2` keep. It reads nothing but what
//! it is handed, so the host tests it (`toyos-libc-copies`).

use alloc::collections::BTreeSet;

use toyos_abi::RawHandle;

const O_CLOEXEC: i32 = 0x80000;

const F_DUPFD: i32 = 0;
const F_GETFD: i32 = 1;
const F_SETFD: i32 = 2;
const F_GETFL: i32 = 3;
const F_SETFL: i32 = 4;
const F_GETLK: i32 = 5;
const F_SETLK: i32 = 6;
const F_SETLKW: i32 = 7;
const F_SETOWN: i32 = 8;
const F_GETOWN: i32 = 9;
const F_DUPFD_CLOEXEC: i32 = 1030;
const FD_CLOEXEC: i32 = 1;

/// What `fcntl` does for one command.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Command {
    /// A duplicate numbered `floor` or above ([`dup_at_least`]).
    DupAtLeast(u32),
    /// The descriptor's close-on-exec flag, read.
    GetFd,
    /// The descriptor's close-on-exec flag, set to this.
    SetFd(bool),
    /// A record lock, which no file here supports, or a command or `F_DUPFD`
    /// floor POSIX refuses: `EINVAL`.
    Invalid,
    /// A descriptor's status flags or owner, or a duplicate closed on `exec`,
    /// none of which this library keeps: `ENOSYS`.
    Unsupported,
}

/// `cmd`, its third argument read from `arg`, the register a variadic one
/// arrives in. `F_DUPFD` and `F_SETFD` take an `int`, which leaves the
/// register's upper half unspecified, so only the lower is read.
pub(crate) fn command(cmd: i32, arg: u64) -> Command {
    let int = arg as u32 as i32;
    match cmd {
        // POSIX's `EINVAL` for a floor at or above `{OPEN_MAX}`, the slots a
        // table has: past them only a slot's generation reaches the floor.
        F_DUPFD => match u32::try_from(int) {
            Ok(floor) if (floor as usize) < RawHandle::MAX_SLOTS => Command::DupAtLeast(floor),
            _ => Command::Invalid,
        },
        F_GETFD => Command::GetFd,
        F_SETFD => Command::SetFd(int & FD_CLOEXEC != 0),
        F_GETLK | F_SETLK | F_SETLKW => Command::Invalid,
        F_DUPFD_CLOEXEC | F_GETFL | F_SETFL | F_GETOWN | F_SETOWN => Command::Unsupported,
        _ => Command::Invalid,
    }
}

/// A duplicate numbered `floor` or above, from `dup`, holding at most one
/// other duplicate at a time. The kernel picks each number, so this is not the
/// lowest free one: a duplicate below a floor under the slot count is a slot at
/// its first generation, and closed again it is the next `dup`'s, one
/// generation on and numbered past every slot.
pub(crate) fn dup_at_least<E>(
    floor: u32,
    mut dup: impl FnMut() -> Result<u32, E>,
    mut close: impl FnMut(u32),
) -> Result<u32, E> {
    loop {
        let duplicate = dup()?;
        if duplicate >= floor {
            return Ok(duplicate);
        }
        close(duplicate);
    }
}

/// The descriptors marked close-on-exec, by number: kept for stage 3 of
/// `issues/kernel/a-childs-end-is-an-event-and-a-parent-takes-its-children-down.md`,
/// whose spawn reads them. A number is a slot at one generation, so a handle
/// made later in the slot is never taken for one marked here.
pub(crate) struct CloseOnExec(BTreeSet<i32>);

impl CloseOnExec {
    pub(crate) const fn new() -> CloseOnExec {
        CloseOnExec(BTreeSet::new())
    }

    /// `fd` as `open` answered it for `flags`: marked if they hold `O_CLOEXEC`.
    pub(crate) fn opened(&mut self, fd: i32, flags: i32) {
        self.set(fd, flags & O_CLOEXEC != 0);
    }

    pub(crate) fn set(&mut self, fd: i32, cloexec: bool) {
        if cloexec {
            self.0.insert(fd);
        } else {
            self.0.remove(&fd);
        }
    }

    /// `F_GETFD`'s answer.
    pub(crate) fn flags(&self, fd: i32) -> i32 {
        if self.0.contains(&fd) { FD_CLOEXEC } else { 0 }
    }
}
