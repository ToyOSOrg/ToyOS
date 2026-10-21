//! What `fcntl` decides before the descriptor table is asked.

use crate::fdreq::{self, Command};
use crate::header::{self, FCNTL_H};

/// What clang leaves in the register of an `int` argument of -1: `movl
/// $0xffffffff, %edx`, which zeroes the upper half (measured on
/// `206_libc_refusals.o`).
const MINUS_ONE_AS_CLANG_PASSES_IT: u64 = 0xffff_ffff;

#[test]
fn fcntl_reads_an_int_argument_from_the_lower_half_of_its_register() {
    let set_cloexec = header::int(FCNTL_H, "FD_CLOEXEC") as u64;
    for (cmd, arg, want) in [
        ("F_DUPFD", MINUS_ONE_AS_CLANG_PASSES_IT, Command::Invalid),
        ("F_DUPFD", u64::MAX, Command::Invalid),
        ("F_DUPFD", 0xdead_beef_0000_000a, Command::DupAtLeast { floor: 10, cloexec: false }),
        ("F_DUPFD", 0, Command::DupAtLeast { floor: 0, cloexec: false }),
        ("F_DUPFD", 4095, Command::DupAtLeast { floor: 4095, cloexec: false }),
        // `{OPEN_MAX}`, the descriptors a table has.
        ("F_DUPFD", 4096, Command::Invalid),
        ("F_DUPFD", 0x7fff_ffff, Command::Invalid),
        ("F_GETFD", u64::MAX, Command::GetFd),
        ("F_SETFD", set_cloexec, Command::SetFd(true)),
        ("F_SETFD", 0xffff_ffff_0000_0000, Command::SetFd(false)),
        ("F_SETFD", 0, Command::SetFd(false)),
        ("F_GETFL", 0, Command::Unsupported),
        ("F_SETFL", 0, Command::Unsupported),
        ("F_GETLK", 0, Command::Invalid),
        ("F_SETLK", 0, Command::Invalid),
        ("F_SETLKW", 0, Command::Invalid),
        ("F_SETOWN", 0, Command::Unsupported),
        ("F_GETOWN", 0, Command::Unsupported),
        ("F_DUPFD_CLOEXEC", 10, Command::DupAtLeast { floor: 10, cloexec: true }),
        ("F_DUPFD_CLOEXEC", 4096, Command::Invalid),
    ] {
        assert_eq!(fdreq::command(header::int(FCNTL_H, cmd), arg), want, "fcntl(_, {cmd}, {arg:#x})");
    }
    for cmd in [12345, -1] {
        assert_eq!(fdreq::command(cmd, 0), Command::Invalid, "fcntl(_, {cmd}, 0)");
    }
}
