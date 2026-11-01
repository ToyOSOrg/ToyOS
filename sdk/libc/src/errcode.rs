//! libc's errno codes, `include/errno.h`'s by name and value, and the one
//! reading of a kernel's or a file server's refusal as one. Nothing here reads or sets `errno`
//! (`errno.rs` does), so the host tests it (`toyos-libc-copies`).

use toyos_abi::syscall::SyscallError;

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
pub(crate) const ECONNABORTED: i32 = 103;
pub(crate) const ECONNRESET: i32 = 104;
pub(crate) const EISCONN: i32 = 106;
pub(crate) const ENOTCONN: i32 = 107;
pub(crate) const ETIMEDOUT: i32 = 110;
pub(crate) const ECONNREFUSED: i32 = 111;
pub(crate) const EHOSTUNREACH: i32 = 113;
pub(crate) const EADDRNOTAVAIL: i32 = 99;
pub(crate) const ESTALE: i32 = 116;

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
        // The kernel's word for a full handle table and for no memory alike.
        SyscallError::ResourceExhausted => ENOMEM,
        SyscallError::NotSupported => EOPNOTSUPP,
        _ => EINVAL,
    }
}

/// Why a program or a library was not read into an image, as POSIX words it.
pub(crate) fn of_image(refused: toyos::process::ImageRefused<toyos::files::Error>) -> i32 {
    use toyos::process::ImageRefused;
    match refused {
        ImageRefused::Empty => ENOEXEC,
        ImageRefused::Shrank => EIO,
        ImageRefused::Memory(e) => of(e),
        ImageRefused::Read(e) => of_files(e),
    }
}

/// The errno a refusal of `toyos::files` is answered with.
pub(crate) fn of_files(e: toyos::files::Error) -> i32 {
    use toyos::files::Error;
    match e {
        Error::Kernel(e) => of(e),
        Error::Served(e) => of_served(e),
        Error::Fd(toyos::fd::Error::BadFd) | Error::NotAFile => EBADF,
        Error::Fd(toyos::fd::Error::Full) => EMFILE,
        Error::NotUtf8 => EILSEQ,
        Error::Loop => ELOOP,
        Error::CrossesServers => EXDEV,
        Error::NotADirectory => ENOTDIR,
        Error::Stream(refused) => of_stream(refused),
    }
}

/// A stream's refusal in POSIX's word: `send`'s, `recv`'s, and `read`'s and
/// `write`'s of a stream's descriptor.
pub(crate) fn of_stream(refused: toyos::net::stream::Refused) -> i32 {
    use toyos::net::stream::Refused;
    match refused {
        Refused::NotThisProcess(_) => EBADF,
        Refused::WouldBlock => EAGAIN,
        Refused::TimedOut | Refused::ConnectionTimedOut => ETIMEDOUT,
        Refused::Reset => ECONNRESET,
        Refused::Unreachable => EHOSTUNREACH,
        Refused::AddressGone => EADDRNOTAVAIL,
        Refused::Aborted => ECONNABORTED,
        Refused::Shut => EPIPE,
    }
}

/// The errno a file server's refusal, or its client end's, is answered with.
fn of_served(e: toyos::fs::Error) -> i32 {
    use toyos::fs::Error;
    match e {
        Error::NotFound | Error::NotHeld => ENOENT,
        Error::AlreadyExists => EEXIST,
        Error::NotADirectory | Error::NoDirectory => ENOTDIR,
        Error::IsADirectory => EISDIR,
        Error::NotEmpty => ENOTEMPTY,
        Error::NotALink | Error::IntoItself | Error::UnknownFile | Error::InvalidPath | Error::InvalidRequest => EINVAL,
        Error::NameTooLong => ENAMETOOLONG,
        Error::InvalidName => EINVAL,
        Error::ReadOnly => EROFS,
        Error::NotOpenForWriting => EBADF,
        Error::Root => EBUSY,
        Error::Escapes => EACCES,
        Error::CrossesDirectories => EXDEV,
        Error::Loop => ELOOP,
        Error::NoSpace => ENOSPC,
        Error::FileTooLarge => EFBIG,
        Error::LimitExceeded | Error::TooManyConnections | Error::TooManyStreams => EAGAIN,
        Error::TooManyOpen => ENFILE,
        Error::Io | Error::Malformed => EIO,
        Error::Stale | Error::Gone => ESTALE,
        Error::Unsupported => EOPNOTSUPP,
        Error::OutOfResources => ENOMEM,
    }
}
