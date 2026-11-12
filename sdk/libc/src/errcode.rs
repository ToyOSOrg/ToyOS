//! libc's errno codes, `include/errno.h`'s by name and value, and the one
//! table from a refusal to one ([`of`]). Nothing here reads or sets `errno`
//! (`errno.rs` does), so the host tests it (`toyos-libc-copies`).

use toyos::Failure;

pub(crate) const EPERM: i32 = 1;
pub(crate) const ENOENT: i32 = 2;
pub(crate) const ESRCH: i32 = 3;
pub(crate) const EINTR: i32 = 4;
pub(crate) const EIO: i32 = 5;
pub(crate) const E2BIG: i32 = 7;
pub(crate) const ENOEXEC: i32 = 8;
pub(crate) const EBADF: i32 = 9;
pub(crate) const ECHILD: i32 = 10;
pub(crate) const EAGAIN: i32 = 11;
pub(crate) const ENOMEM: i32 = 12;
pub(crate) const EACCES: i32 = 13;
pub(crate) const EFAULT: i32 = 14;
pub(crate) const EBUSY: i32 = 16;
pub(crate) const EEXIST: i32 = 17;
pub(crate) const EXDEV: i32 = 18;
pub(crate) const ENODEV: i32 = 19;
pub(crate) const ENOTDIR: i32 = 20;
pub(crate) const EISDIR: i32 = 21;
pub(crate) const EINVAL: i32 = 22;
pub(crate) const ENFILE: i32 = 23;
pub(crate) const EMFILE: i32 = 24;
pub(crate) const ENOTTY: i32 = 25;
pub(crate) const ETXTBSY: i32 = 26;
pub(crate) const EFBIG: i32 = 27;
pub(crate) const ENOSPC: i32 = 28;
pub(crate) const EROFS: i32 = 30;
pub(crate) const EPIPE: i32 = 32;
pub(crate) const EDOM: i32 = 33;
pub(crate) const ERANGE: i32 = 34;
pub(crate) const EDEADLK: i32 = 35;
pub(crate) const ENAMETOOLONG: i32 = 36;
pub(crate) const ENOSYS: i32 = 38;
pub(crate) const ENOTEMPTY: i32 = 39;
pub(crate) const ELOOP: i32 = 40;
pub(crate) const EOVERFLOW: i32 = 75;
pub(crate) const EILSEQ: i32 = 84;
pub(crate) const ENOPROTOOPT: i32 = 92;
pub(crate) const EOPNOTSUPP: i32 = 95;
pub(crate) const EAFNOSUPPORT: i32 = 97;
pub(crate) const EADDRINUSE: i32 = 98;
pub(crate) const ENETUNREACH: i32 = 101;
pub(crate) const ECONNABORTED: i32 = 103;
pub(crate) const ECONNRESET: i32 = 104;
pub(crate) const EISCONN: i32 = 106;
pub(crate) const ENOTCONN: i32 = 107;
pub(crate) const ETIMEDOUT: i32 = 110;
pub(crate) const ECONNREFUSED: i32 = 111;
pub(crate) const EHOSTUNREACH: i32 = 113;
pub(crate) const EADDRNOTAVAIL: i32 = 99;
pub(crate) const ESTALE: i32 = 116;

/// The errno a refusal is answered with: the kernel's, a file server's,
/// netstack's or the SDK's own, each read as its [`Failure`] first, so every
/// call answers one refusal with one word. A call whose POSIX page names
/// another word for a refusal answers that itself, by the refusal's name.
pub(crate) fn of(e: impl Into<Failure>) -> i32 {
    match e.into() {
        Failure::NotFound => ENOENT,
        Failure::PermissionDenied => EACCES,
        Failure::AlreadyExists => EEXIST,
        Failure::InvalidArgument | Failure::InvalidName => EINVAL,
        Failure::BadAddress => EFAULT,
        Failure::WouldBlock | Failure::Quota | Failure::TryAgain => EAGAIN,
        Failure::OutOfMemory => ENOMEM,
        Failure::Unsupported => EOPNOTSUPP,
        // `Unknown` too: POSIX has no word for a refusal that names no reason.
        Failure::Io | Failure::Unknown | Failure::Protocol | Failure::Truncated => EIO,
        // A write whose reader is gone, and not `ENOENT`, which says the file does not exist.
        Failure::BrokenPipe => EPIPE,
        Failure::BadDescriptor | Failure::WrongDescriptor | Failure::NotOpenForWriting => EBADF,
        Failure::TooManyDescriptors => EMFILE,
        Failure::NotUtf8 => EILSEQ,
        Failure::Loop => ELOOP,
        Failure::CrossesDevices => EXDEV,
        Failure::NotADirectory => ENOTDIR,
        Failure::IsADirectory => EISDIR,
        Failure::NotEmpty => ENOTEMPTY,
        Failure::NameTooLong => ENAMETOOLONG,
        Failure::ReadOnly => EROFS,
        Failure::Busy => EBUSY,
        Failure::TextBusy => ETXTBSY,
        Failure::NoSpace => ENOSPC,
        Failure::FileTooLarge => EFBIG,
        Failure::TooManyOpenFiles => ENFILE,
        Failure::Stale => ESTALE,
        Failure::TimedOut => ETIMEDOUT,
        Failure::ConnectionRefused => ECONNREFUSED,
        Failure::ConnectionReset => ECONNRESET,
        Failure::ConnectionAborted => ECONNABORTED,
        Failure::NotConnected => ENOTCONN,
        Failure::AddrInUse => EADDRINUSE,
        Failure::AddrNotAvailable => EADDRNOTAVAIL,
        Failure::HostUnreachable => EHOSTUNREACH,
        Failure::NetworkUnreachable => ENETUNREACH,
        Failure::NotExecutable => ENOEXEC,
        Failure::TooBig => E2BIG,
    }
}

/// `getaddrinfo`'s words, `include/netdb.h`'s by name and value.
pub(crate) mod eai {
    pub(crate) const NONAME: i32 = -2;
    pub(crate) const AGAIN: i32 = -3;
    pub(crate) const FAIL: i32 = -4;
    pub(crate) const MEMORY: i32 = -10;
    pub(crate) const SYSTEM: i32 = -11;
}

/// What `getaddrinfo` answers for a lookup's refusal: a word of its own, or
/// [`eai::SYSTEM`] and the errno it is to set beside it.
pub(crate) fn of_lookup(e: impl Into<Failure>) -> (i32, Option<i32>) {
    match e.into() {
        Failure::TryAgain | Failure::TimedOut | Failure::Quota | Failure::WouldBlock => (eai::AGAIN, None),
        Failure::OutOfMemory => (eai::MEMORY, None),
        Failure::InvalidArgument => (eai::NONAME, None),
        // The resolver answered, and not with the name.
        Failure::Io | Failure::Protocol => (eai::FAIL, None),
        other => (eai::SYSTEM, Some(of(other))),
    }
}
