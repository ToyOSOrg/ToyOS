//! libc's one list of errno codes (`errcode.rs`) against `include/errno.h`, the
//! numbers a C program compares them with: every code the library answers
//! with is the header's, by name and value.

use toyos::net::NetError;
use toyos::{fd, files, fs, Failure};
use toyos_abi::syscall::SyscallError;

use crate::errcode::{self, eai, of};
use crate::header::{self, ERRNO_H, NETDB_H};

#[test]
fn every_code_libc_answers_with_is_errno_h_s() {
    let list = include_str!("../../src/errcode.rs");
    let codes: Vec<(&str, i32)> = list
        .lines()
        .filter_map(|line| line.strip_prefix("pub(crate) const "))
        .map(|rest| {
            let (name, value) = rest.split_once(": i32 = ").unwrap_or_else(|| panic!("errcode.rs: {rest}"));
            (name, value.trim_end_matches(';').parse().unwrap_or_else(|_| panic!("errcode.rs: {rest}")))
        })
        .collect();
    assert!(codes.len() > 20, "errcode.rs lists {} codes", codes.len());
    for (name, value) in codes {
        assert_eq!(header::int(ERRNO_H, name), value, "{name}");
    }
}

/// Every failure has an errno the header defines. That each has one at all is
/// `of`'s exhaustive match; this holds that none is a number C has no name for.
#[test]
fn every_failure_s_errno_is_one_errno_h_defines() {
    let defined: Vec<i32> = ERRNO_H
        .lines()
        .filter_map(|line| line.strip_prefix("#define E")?.split_whitespace().nth(1)?.parse().ok())
        .collect();
    assert!(defined.len() > 40, "errno.h defines {} codes", defined.len());
    for &failure in Failure::ALL {
        assert!(defined.contains(&of(failure)), "{failure:?} is errno {}", of(failure));
    }
}

/// The kernel's words as POSIX names them: none is the `EINVAL` an argument
/// alone earns, and a device's failure alone is `EIO`'s.
#[test]
fn a_kernel_refusal_is_its_own_errno() {
    use SyscallError::*;
    for (word, errno) in [
        (NotFound, "ENOENT"),
        (PermissionDenied, "EACCES"),
        (AlreadyExists, "EEXIST"),
        (InvalidArgument, "EINVAL"),
        (BadAddress, "EFAULT"),
        (WouldBlock, "EAGAIN"),
        (ResourceExhausted, "ENOMEM"),
        (NotSupported, "EOPNOTSUPP"),
        (Io, "EIO"),
        (Gone, "EPIPE"),
        (Busy, "EBUSY"),
        (TextBusy, "ETXTBSY"),
        (Unknown, "EIO"),
    ] {
        assert_eq!(of(word), header::int(ERRNO_H, errno), "{word:?}");
    }
}

/// The descriptor table's, a file server's and netstack's refusals, each in
/// the word POSIX's page for the call has.
#[test]
fn a_refusal_above_the_kernel_is_its_own_errno() {
    let named = |errno: &str| header::int(ERRNO_H, errno);
    assert_eq!(of(fd::Error::BadFd), named("EBADF"));
    assert_eq!(of(fd::Error::Full), named("EMFILE"));
    assert_eq!(of(files::Error::Fd(fd::Error::Full)), named("EMFILE"));
    assert_eq!(of(files::Error::Kernel(SyscallError::Gone)), named("EPIPE"));
    assert_eq!(of(files::Error::NotAFile), named("EBADF"));
    for (word, errno) in [
        (fs::Error::NotEmpty, "ENOTEMPTY"),
        (fs::Error::IsADirectory, "EISDIR"),
        (fs::Error::NameTooLong, "ENAMETOOLONG"),
        (fs::Error::ReadOnly, "EROFS"),
        (fs::Error::NoSpace, "ENOSPC"),
        (fs::Error::TooManyOpen, "ENFILE"),
        (fs::Error::Unsupported, "EOPNOTSUPP"),
        (fs::Error::Stale, "ESTALE"),
        (fs::Error::Gone, "ESTALE"),
    ] {
        assert_eq!(of(word), named(errno), "{word:?}");
    }
    for (word, errno) in [
        (NetError::ConnectionRefused, "ECONNREFUSED"),
        (NetError::NetworkUnreachable, "ENETUNREACH"),
        (NetError::HostUnreachable, "EHOSTUNREACH"),
        (NetError::NetstackNotFound, "ENOTCONN"),
        (NetError::ResourceExhausted, "EAGAIN"),
        (NetError::Io, "EIO"),
    ] {
        assert_eq!(of(word), named(errno), "{word:?}");
    }
}

/// `getaddrinfo`'s words are the header's, and a lookup's refusal is the one
/// POSIX gives it: to ask again, a failed resolver, or errno's.
#[test]
fn a_lookup_s_refusal_is_getaddrinfo_s_word() {
    for (name, value) in [
        ("EAI_NONAME", eai::NONAME),
        ("EAI_AGAIN", eai::AGAIN),
        ("EAI_FAIL", eai::FAIL),
        ("EAI_MEMORY", eai::MEMORY),
        ("EAI_SYSTEM", eai::SYSTEM),
    ] {
        assert_eq!(header::int(NETDB_H, name), value, "{name}");
    }
    assert_eq!(errcode::of_lookup(NetError::TryAgain), (eai::AGAIN, None));
    assert_eq!(errcode::of_lookup(NetError::TimedOut), (eai::AGAIN, None));
    assert_eq!(errcode::of_lookup(NetError::Io), (eai::FAIL, None));
    assert_eq!(
        errcode::of_lookup(NetError::NetworkUnreachable),
        (eai::SYSTEM, Some(header::int(ERRNO_H, "ENETUNREACH")))
    );
}
