//! Why a call failed, in the one vocabulary a runtime translates.
//!
//! The kernel, a file server, netstack and this crate's own tables each refuse
//! in words of their own. Every one of those words is read here, by name, as a
//! [`Failure`], and a runtime keeps one table from `Failure` to its error word:
//! libc's to `errno`, std's to `io::ErrorKind`. A word added to any vocabulary
//! fails to compile here until it is given its `Failure`, and a `Failure`
//! added fails to compile in each runtime until it is given its word, so no
//! refusal reaches a caller as a word some table fell through to.
//!
//! A variant exists where a runtime's word differs: two refusals that every
//! runtime answers alike are one `Failure`.

use toyos_abi::syscall::SyscallError;

use crate::net::stream::Refused as StreamRefused;
use crate::net::NetError;
use crate::process::ImageRefused;
use crate::{dl, fd, files, fs, spawn};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// No file, name or object by that name.
    NotFound,
    PermissionDenied,
    AlreadyExists,
    InvalidArgument,
    /// A pointer the kernel could not read or write through.
    BadAddress,
    /// The call does not wait, and what it waits for is not there.
    WouldBlock,
    OutOfMemory,
    /// The object or the volume has no such operation.
    Unsupported,
    /// A device did not do it, or what it holds does not decode.
    Io,
    /// The other end of a pipe, a connection or a stream's sending half is gone.
    BrokenPipe,
    /// A refusal that names no reason: a kernel word this SDK does not know.
    Unknown,
    /// No open descriptor by that number.
    BadDescriptor,
    /// An open descriptor, of an object the call is not made on.
    WrongDescriptor,
    /// The process holds as many descriptors as it may.
    TooManyDescriptors,
    /// A path that is not UTF-8.
    NotUtf8,
    /// More links on the way than are followed.
    Loop,
    /// A rename or a link across two filesystem authorities.
    CrossesDevices,
    NotADirectory,
    IsADirectory,
    NotEmpty,
    NameTooLong,
    /// A name the volume cannot store.
    InvalidName,
    /// The directory's grant, or its volume, lets nothing in it change.
    ReadOnly,
    /// A write to a file not opened to write.
    NotOpenForWriting,
    /// It is in use as something that stays where it is.
    Busy,
    /// A file a program runs from, which is not written while it does.
    TextBusy,
    NoSpace,
    FileTooLarge,
    /// Past a bound a server keeps, which clears when something is let go.
    Quota,
    /// A server holds as many files open as it keeps.
    TooManyOpenFiles,
    /// A peer answered outside its protocol.
    Protocol,
    /// The file was removed or replaced, or its server restarted or ended,
    /// since it was opened.
    Stale,
    TimedOut,
    ConnectionRefused,
    ConnectionReset,
    /// netstack ended the connection, neither the peer's reset nor its end.
    ConnectionAborted,
    NotConnected,
    AddrInUse,
    AddrNotAvailable,
    HostUnreachable,
    NetworkUnreachable,
    /// A lookup that ended without an answer and may have one if asked again.
    TryAgain,
    /// A file that is no program or library this machine loads.
    NotExecutable,
    /// A file ended before the length it was read at.
    Truncated,
    /// A spawn carrying more than one carries.
    TooBig,
}

impl Failure {
    /// Every failure, for a runtime's test of its own table.
    pub const ALL: &'static [Self] = {
        use Failure::*;
        &[
            NotFound, PermissionDenied, AlreadyExists, InvalidArgument, BadAddress, WouldBlock, OutOfMemory,
            Unsupported, Io, BrokenPipe, Unknown, BadDescriptor, WrongDescriptor, TooManyDescriptors, NotUtf8, Loop,
            CrossesDevices, NotADirectory, IsADirectory, NotEmpty, NameTooLong, InvalidName, ReadOnly,
            NotOpenForWriting, Busy, NoSpace, FileTooLarge, Quota, TooManyOpenFiles, Protocol, Stale, TimedOut,
            ConnectionRefused, ConnectionReset, ConnectionAborted, NotConnected, AddrInUse, AddrNotAvailable,
            HostUnreachable, NetworkUnreachable, TryAgain, NotExecutable, Truncated, TooBig,
        ]
    };
}

impl From<SyscallError> for Failure {
    fn from(e: SyscallError) -> Self {
        match e {
            SyscallError::Unknown => Self::Unknown,
            SyscallError::NotFound => Self::NotFound,
            SyscallError::PermissionDenied => Self::PermissionDenied,
            SyscallError::AlreadyExists => Self::AlreadyExists,
            SyscallError::InvalidArgument => Self::InvalidArgument,
            SyscallError::BadAddress => Self::BadAddress,
            SyscallError::WouldBlock => Self::WouldBlock,
            // The kernel's word for no memory and for a full handle table alike.
            SyscallError::ResourceExhausted => Self::OutOfMemory,
            SyscallError::NotSupported => Self::Unsupported,
            SyscallError::Io => Self::Io,
            SyscallError::Gone => Self::BrokenPipe,
            SyscallError::Busy => Self::Busy,
            SyscallError::TextBusy => Self::TextBusy,
        }
    }
}

impl From<fd::Error> for Failure {
    fn from(e: fd::Error) -> Self {
        match e {
            fd::Error::BadFd => Self::BadDescriptor,
            fd::Error::Full => Self::TooManyDescriptors,
        }
    }
}

impl From<fs::Error> for Failure {
    fn from(e: fs::Error) -> Self {
        use fs::Error;
        match e {
            Error::NotFound | Error::NotHeld => Self::NotFound,
            Error::AlreadyExists => Self::AlreadyExists,
            Error::NotADirectory | Error::NoDirectory => Self::NotADirectory,
            Error::IsADirectory => Self::IsADirectory,
            Error::NotEmpty => Self::NotEmpty,
            Error::NotALink | Error::IntoItself | Error::UnknownFile | Error::InvalidPath | Error::InvalidRequest => {
                Self::InvalidArgument
            }
            Error::NameTooLong => Self::NameTooLong,
            Error::InvalidName => Self::InvalidName,
            Error::ReadOnly => Self::ReadOnly,
            Error::NotOpenForWriting => Self::NotOpenForWriting,
            Error::Root => Self::Busy,
            Error::Escapes => Self::PermissionDenied,
            Error::CrossesDirectories => Self::CrossesDevices,
            Error::Loop => Self::Loop,
            Error::NoSpace => Self::NoSpace,
            Error::FileTooLarge => Self::FileTooLarge,
            Error::LimitExceeded | Error::TooManyConnections | Error::TooManyStreams => Self::Quota,
            Error::TooManyOpen => Self::TooManyOpenFiles,
            Error::Io => Self::Io,
            Error::Stale | Error::Gone => Self::Stale,
            Error::Unsupported => Self::Unsupported,
            Error::OutOfResources => Self::OutOfMemory,
            Error::Malformed => Self::Protocol,
        }
    }
}

impl From<files::Error> for Failure {
    fn from(e: files::Error) -> Self {
        use files::Error;
        match e {
            Error::Kernel(e) => e.into(),
            Error::Served(e) => e.into(),
            Error::Fd(e) => e.into(),
            Error::NotUtf8 => Self::NotUtf8,
            Error::Loop => Self::Loop,
            Error::CrossesServers => Self::CrossesDevices,
            Error::NotAFile => Self::WrongDescriptor,
            Error::Stream(refused) => refused.into(),
            Error::NotADirectory => Self::NotADirectory,
        }
    }
}

impl From<StreamRefused> for Failure {
    fn from(refused: StreamRefused) -> Self {
        match refused {
            StreamRefused::NotThisProcess(_) => Self::WrongDescriptor,
            StreamRefused::WouldBlock => Self::WouldBlock,
            StreamRefused::TimedOut | StreamRefused::ConnectionTimedOut => Self::TimedOut,
            StreamRefused::Reset => Self::ConnectionReset,
            StreamRefused::Unreachable => Self::HostUnreachable,
            StreamRefused::AddressGone => Self::AddrNotAvailable,
            StreamRefused::Aborted => Self::ConnectionAborted,
            StreamRefused::Shut => Self::BrokenPipe,
        }
    }
}

impl From<NetError> for Failure {
    fn from(e: NetError) -> Self {
        match e {
            // The machine has no network, as `ERR_NOT_CONNECTED` says of one whose netstack has no link.
            NetError::NetstackNotFound | NetError::NotConnected => Self::NotConnected,
            NetError::ConnectionRefused => Self::ConnectionRefused,
            NetError::ConnectionReset => Self::ConnectionReset,
            NetError::TimedOut => Self::TimedOut,
            NetError::AddrInUse => Self::AddrInUse,
            NetError::InvalidInput => Self::InvalidArgument,
            NetError::ResourceExhausted => Self::Quota,
            NetError::PermissionDenied => Self::PermissionDenied,
            NetError::WouldBlock => Self::WouldBlock,
            NetError::NotASocket => Self::WrongDescriptor,
            NetError::NetworkUnreachable => Self::NetworkUnreachable,
            NetError::HostUnreachable => Self::HostUnreachable,
            NetError::TryAgain => Self::TryAgain,
            NetError::Protocol(_) => Self::Protocol,
            NetError::Io => Self::Io,
        }
    }
}

impl<E: Into<Failure>> From<ImageRefused<E>> for Failure {
    fn from(refused: ImageRefused<E>) -> Self {
        match refused {
            ImageRefused::Empty => Self::NotExecutable,
            ImageRefused::Memory(e) => e.into(),
            ImageRefused::Shrank => Self::Truncated,
            ImageRefused::Read(e) => e.into(),
        }
    }
}

impl From<dl::Refused> for Failure {
    fn from(refused: dl::Refused) -> Self {
        match refused {
            dl::Refused::Read(_, why) => why.into(),
            dl::Refused::Memory(e) => e.into(),
            dl::Refused::Elf(_) | dl::Refused::Relocation(_) | dl::Refused::Undefined(_) => Self::NotExecutable,
        }
    }
}

impl From<&spawn::Refused> for Failure {
    fn from(refused: &spawn::Refused) -> Self {
        use spawn::Refused;
        match *refused {
            Refused::Permission(_) => Self::PermissionDenied,
            Refused::Invalid(_) => Self::TooBig,
            Refused::Stream(_) => Self::WrongDescriptor,
            // A holder past what netstack has places for is a descriptor too many.
            Refused::Export(NetError::ResourceExhausted) => Self::TooManyDescriptors,
            Refused::Export(e) => e.into(),
            Refused::File(e) => e.into(),
            // The launcher's answer was lost, or it started nothing it permits.
            Refused::NotStarted => Self::Io,
            Refused::Syscall(e) => e.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_failure_is_listed_once() {
        for (i, failure) in Failure::ALL.iter().enumerate() {
            assert!(!Failure::ALL[..i].contains(failure), "{failure:?} is listed twice");
        }
        // A variant added and not listed fails here by its discriminant.
        assert_eq!(Failure::ALL.len(), Failure::TooBig as usize + 1);
        for (i, failure) in Failure::ALL.iter().enumerate() {
            assert_eq!(*failure as usize, i);
        }
    }

    /// Every word the kernel's ABI decodes, a code it does not know among
    /// them, is a failure.
    #[test]
    fn every_kernel_word_is_a_failure() {
        for code in 0..=255u64 {
            let word = SyscallError::from_u64(u64::MAX - code).expect("an error's code");
            let failure = Failure::from(word);
            assert_eq!(failure == Failure::Unknown, word == SyscallError::Unknown, "{word:?}");
        }
        assert_eq!(Failure::from(SyscallError::Gone), Failure::BrokenPipe);
        assert_eq!(Failure::from(SyscallError::BadAddress), Failure::BadAddress);
        assert_eq!(Failure::from(SyscallError::NotSupported), Failure::Unsupported);
        assert_eq!(Failure::from(SyscallError::Io), Failure::Io);
    }

    /// Every code a file server sends, and every word of the client end's own.
    #[test]
    fn every_file_protocol_word_is_a_failure() {
        let mut words = 0;
        for byte in 0..=u8::MAX {
            if let Some(word) = fs::Error::from_byte(byte) {
                let _ = Failure::from(word);
                words += 1;
            }
        }
        assert!(words > 30, "the protocol's words: {words}");
        assert_eq!(Failure::from(fs::Error::NotEmpty), Failure::NotEmpty);
        assert_eq!(Failure::from(fs::Error::Gone), Failure::Stale);
        assert_eq!(Failure::from(files::Error::Served(fs::Error::NoSpace)), Failure::NoSpace);
        assert_eq!(Failure::from(files::Error::Fd(fd::Error::Full)), Failure::TooManyDescriptors);
    }

    /// Every code netstack sends, and one it does not.
    #[test]
    fn every_pipe_word_is_a_failure() {
        use crate::net::*;
        for (code, failure) in [
            (ERR_CONNECTION_REFUSED, Failure::ConnectionRefused),
            (ERR_CONNECTION_RESET, Failure::ConnectionReset),
            (ERR_TIMED_OUT, Failure::TimedOut),
            (ERR_ADDR_IN_USE, Failure::AddrInUse),
            (ERR_NOT_CONNECTED, Failure::NotConnected),
            (ERR_INVALID_INPUT, Failure::InvalidArgument),
            (ERR_RESOURCE_EXHAUSTED, Failure::Quota),
            (ERR_PERMISSION_DENIED, Failure::PermissionDenied),
            (ERR_WOULD_BLOCK, Failure::WouldBlock),
            (ERR_NETWORK_UNREACHABLE, Failure::NetworkUnreachable),
            (ERR_HOST_UNREACHABLE, Failure::HostUnreachable),
            (ERR_TRY_AGAIN, Failure::TryAgain),
            (ERR_OTHER, Failure::Io),
            (4242, Failure::Protocol),
        ] {
            assert_eq!(Failure::from(NetError::from_error_code(code)), failure, "code {code}");
        }
    }

    #[test]
    fn a_spawn_refused_a_holder_is_a_descriptor_too_many() {
        use spawn::Refused;
        assert_eq!(Failure::from(&Refused::Export(NetError::ResourceExhausted)), Failure::TooManyDescriptors);
        assert_eq!(Failure::from(&Refused::Export(NetError::NetstackNotFound)), Failure::NotConnected);
        assert_eq!(Failure::from(&Refused::Syscall(SyscallError::NotFound)), Failure::NotFound);
    }
}
