//! What `fcntl` decides before the descriptor table is asked. It reads
//! nothing but what it is handed, so the host tests it (`toyos-libc-copies`).

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
pub(crate) const FD_CLOEXEC: i32 = 1;

/// `{OPEN_MAX}`: the descriptors one table holds (`toyos::fd::MAX_FDS`).
pub(crate) const OPEN_MAX: usize = 4096;

/// What `fcntl` does for one command.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Command {
    /// The lowest free descriptor at or above `floor` on the same object,
    /// marked close-on-exec or not.
    DupAtLeast { floor: i32, cloexec: bool },
    /// The descriptor's close-on-exec flag, read.
    GetFd,
    /// The descriptor's close-on-exec flag, set to this.
    SetFd(bool),
    /// A record lock, which no file here supports, or a command or `F_DUPFD`
    /// floor POSIX refuses: `EINVAL`.
    Invalid,
    /// A descriptor's status flags or owner, none of which this library
    /// keeps: `ENOSYS`.
    Unsupported,
}

/// `cmd`, its third argument read from `arg`, the register a variadic one
/// arrives in. `F_DUPFD` and `F_SETFD` take an `int`, which leaves the
/// register's upper half unspecified, so only the lower is read.
pub(crate) fn command(cmd: i32, arg: u64) -> Command {
    let int = arg as u32 as i32;
    let dup = |cloexec| match usize::try_from(int) {
        // POSIX's `EINVAL` for a floor at or above `{OPEN_MAX}`.
        Ok(floor) if floor < OPEN_MAX => Command::DupAtLeast { floor: int, cloexec },
        _ => Command::Invalid,
    };
    match cmd {
        F_DUPFD => dup(false),
        F_DUPFD_CLOEXEC => dup(true),
        F_GETFD => Command::GetFd,
        F_SETFD => Command::SetFd(int & FD_CLOEXEC != 0),
        F_GETLK | F_SETLK | F_SETLKW => Command::Invalid,
        F_GETFL | F_SETFL | F_GETOWN | F_SETOWN => Command::Unsupported,
        _ => Command::Invalid,
    }
}
