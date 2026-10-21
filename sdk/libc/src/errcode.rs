//! libc's errno codes, `include/errno.h`'s by name and value, and the one
//! reading of a kernel refusal as one. Nothing here reads or sets `errno`
//! (`errno.rs` does), so the host tests it (`toyos-libc-copies`).

use toyos_abi::syscall::SyscallError;

pub(crate) const EPERM: i32 = 1;
pub(crate) const ENOENT: i32 = 2;
pub(crate) const ESRCH: i32 = 3;
pub(crate) const EIO: i32 = 5;
pub(crate) const E2BIG: i32 = 7;
pub(crate) const EBADF: i32 = 9;
pub(crate) const ECHILD: i32 = 10;
pub(crate) const EAGAIN: i32 = 11;
pub(crate) const ENOMEM: i32 = 12;
pub(crate) const EACCES: i32 = 13;
pub(crate) const EFAULT: i32 = 14;
pub(crate) const EBUSY: i32 = 16;
pub(crate) const EEXIST: i32 = 17;
pub(crate) const ENODEV: i32 = 19;
pub(crate) const ENOTDIR: i32 = 20;
pub(crate) const EINVAL: i32 = 22;
pub(crate) const EMFILE: i32 = 24;
pub(crate) const ENOTTY: i32 = 25;
pub(crate) const ENOSPC: i32 = 28;
pub(crate) const EPIPE: i32 = 32;
pub(crate) const EDOM: i32 = 33;
pub(crate) const ERANGE: i32 = 34;
pub(crate) const EDEADLK: i32 = 35;
pub(crate) const ENAMETOOLONG: i32 = 36;
pub(crate) const ENOSYS: i32 = 38;
pub(crate) const ELOOP: i32 = 40;
pub(crate) const EOVERFLOW: i32 = 75;
pub(crate) const EILSEQ: i32 = 84;
pub(crate) const ENOPROTOOPT: i32 = 92;
pub(crate) const EOPNOTSUPP: i32 = 95;
pub(crate) const EAFNOSUPPORT: i32 = 97;
pub(crate) const EADDRINUSE: i32 = 98;
pub(crate) const EPROTO: i32 = 71;
pub(crate) const ECONNABORTED: i32 = 103;
pub(crate) const ECONNRESET: i32 = 104;
pub(crate) const ENOTCONN: i32 = 107;
pub(crate) const ETIMEDOUT: i32 = 110;
pub(crate) const ECONNREFUSED: i32 = 111;
pub(crate) const EHOSTUNREACH: i32 = 113;

/// The errno a kernel refusal is answered with.
pub(crate) fn of(e: SyscallError) -> i32 {
    match e {
        SyscallError::NotFound => ENOENT,
        SyscallError::PermissionDenied => EACCES,
        SyscallError::AlreadyExists => EEXIST,
        SyscallError::InvalidArgument => EINVAL,
        SyscallError::WouldBlock => EAGAIN,
        // A write whose reader is gone, and not `ENOENT`, which says the file does not exist.
        SyscallError::Gone => EPIPE,
        SyscallError::Io => EIO,
        // A read of a connection met handles no frame announced: the peer broke its protocol.
        SyscallError::HandlesPending => EPROTO,
        _ => EINVAL,
    }
}
