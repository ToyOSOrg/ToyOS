//! What a client may take of the server, and what a request's words may name,
//! decided before the server does anything for it.
//!
//! **One share cannot take the server.** Beneath each machine-wide bound —
//! connections waiting on their hello, connections served, streams — each
//! share a grant names has a quarter of its own, so one holding all it may
//! leaves the rest of each bound to three more. Past a bound a hello is
//! answered `TooManyConnections` and a stream `TooManyStreams`, and a
//! connection taken past its share's part of the handshakes is answered
//! `TooManyConnections` and let go.
//!
//! **Every client number is bounded where it is kept**: an offset a request
//! names, and an offset a stream grows by what it drains, stay at or below the
//! largest a file has ([`MAX_FILE_BYTES`]).
//!
//! **A path is the window's bytes the request names, UTF-8**, and one the
//! server resolves is canonical ([`toyos::fs::canonical`]): a `..` a client
//! puts on the wire past std is refused before anything is looked up. A
//! link's target is stored as it was written, never resolved here, so it is
//! held to UTF-8 alone.

use toyos::fs::{canonical, Error, MAX_FILE_BYTES, MAX_PATH, WINDOW_BYTES};
use toyos::shm::Lent;

/// Clients served at once, machine-wide: one connection per directory per
/// process.
pub const MAX_SERVED: usize = 128;

/// Connections taken and not yet answered at their hello. While this many
/// wait, the next waits in the port's queue.
pub const MAX_HANDSHAKES: usize = 32;

/// Streams served at once, machine-wide.
pub const MAX_STREAMS: usize = 64;

/// One share's parts of [`MAX_SERVED`], [`MAX_HANDSHAKES`] and [`MAX_STREAMS`].
pub const SERVED_SHARE: usize = MAX_SERVED / 4;
pub const HANDSHAKE_SHARE: usize = MAX_HANDSHAKES / 4;
pub const STREAM_SHARE: usize = MAX_STREAMS / 4;

/// How a hello is answered.
#[derive(Debug, PartialEq, Eq)]
pub enum Hello<W> {
    /// Served, through the window it lent.
    Served(W),
    /// Answered `TooManyConnections` and let go, for this reason.
    Refused(String),
    /// Let go unanswered, for this reason.
    Dropped(&'static str),
}

/// A connection just taken, while `waiting` of its share's are not yet
/// answered at their hello: `Err` is why it is answered `TooManyConnections`
/// and let go.
pub fn handshake(share: u64, waiting: usize) -> Result<(), String> {
    match waiting >= HANDSHAKE_SHARE {
        true => Err(format!("share {share} has {HANDSHAKE_SHARE} waiting on their hello")),
        false => Ok(()),
    }
}

/// A hello, while `served` clients are served and `of_share` of them are its
/// share's. `window` is the region it lent as the kernel describes it, `None`
/// for a hello that lent none, and is asked only once the bounds have passed.
pub fn hello<W>(served: usize, of_share: usize, window: impl FnOnce() -> Option<Result<W, Lent>>) -> Hello<W> {
    if served >= MAX_SERVED {
        return Hello::Refused(format!("it is refused, since {MAX_SERVED} clients are served already"));
    }
    if of_share >= SERVED_SHARE {
        return Hello::Refused(format!("it is refused, since its share is served {SERVED_SHARE} clients already"));
    }
    match window() {
        None => Hello::Dropped("its hello carried no window"),
        Some(Ok(window)) => Hello::Served(window),
        Some(Err(Lent::Kernel(_))) => Hello::Dropped("its window would not map"),
        Some(Err(Lent::NotRam)) => Hello::Dropped("its window is not memory the kernel allocated"),
        Some(Err(Lent::Length { .. })) => Hello::Dropped("its window is not WINDOW_BYTES long"),
    }
}

/// The end of `len` bytes from `at` in a file, at or below the largest offset
/// a file has, or `FileTooLarge`.
pub fn in_a_file(at: u64, len: u64) -> Result<u64, Error> {
    at.checked_add(len).filter(|&end| end <= MAX_FILE_BYTES).ok_or(Error::FileTooLarge)
}

/// A stream asked to start at `at`, while `streams` are served and `of_share`
/// of them are its share's.
pub fn stream(at: u64, streams: usize, of_share: usize) -> Result<(), Error> {
    in_a_file(at, 0)?;
    match streams >= MAX_STREAMS || of_share >= STREAM_SHARE {
        true => Err(Error::TooManyStreams),
        false => Ok(()),
    }
}

/// Where in the window a path of `len` bytes at `at` is: `NameTooLong` for
/// one longer than a path, `InvalidPath` for one past the window.
pub fn path_span(at: u64, len: u64) -> Result<(usize, usize), Error> {
    if len > MAX_PATH as u64 {
        return Err(Error::NameTooLong);
    }
    let len = len as usize;
    match usize::try_from(at) {
        Ok(at) if at.checked_add(len).is_some_and(|end| end <= WINDOW_BYTES) => Ok((at, len)),
        _ => Err(Error::InvalidPath),
    }
}

/// The path a request's bytes spell: UTF-8, or `InvalidPath`.
pub fn path(bytes: Vec<u8>) -> Result<String, Error> {
    String::from_utf8(bytes).map_err(|_| Error::InvalidPath)
}

/// `rel`, a path the server resolves from a client's directory: canonical, or
/// `InvalidPath`.
pub fn relative(rel: &str) -> Result<(), Error> {
    match canonical(rel) {
        true => Ok(()),
        false => Err(Error::InvalidPath),
    }
}

#[cfg(test)]
mod tests {
    use toyos_abi::syscall::SyscallError;

    use super::*;

    /// A hello whose window nothing asks for: the bounds alone answer it.
    fn unasked() -> Option<Result<(), Lent>> {
        panic!("a hello past a bound was asked for its window")
    }

    #[test]
    fn a_share_at_its_part_of_the_handshakes_is_refused_and_one_below_is_taken() {
        assert_eq!(handshake(7, HANDSHAKE_SHARE - 1), Ok(()));
        assert_eq!(handshake(7, HANDSHAKE_SHARE), Err(format!("share 7 has {HANDSHAKE_SHARE} waiting on their hello")));
    }

    #[test]
    fn a_hello_past_the_server_s_bound_or_its_share_s_part_is_refused_before_its_window_is_asked() {
        assert!(matches!(hello(MAX_SERVED, 0, unasked), Hello::Refused(why) if why.contains("128 clients are served")));
        assert!(matches!(hello(SERVED_SHARE, SERVED_SHARE, unasked), Hello::Refused(why) if why.contains("its share is served 32")));
        assert_eq!(hello(MAX_SERVED - 1, SERVED_SHARE - 1, || Some(Ok(()))), Hello::Served(()));
    }

    /// The kernel's answer for a lent region that is not one region of
    /// `WINDOW_BYTES` lets the client go unanswered, whatever it says it lent:
    /// a device's registers, another kind of object, another length, or none.
    #[test]
    fn a_hello_whose_window_is_no_region_of_window_bytes_is_let_go_unanswered() {
        let refused: [(Option<Result<(), Lent>>, &str); 4] = [
            (Some(Err(Lent::NotRam)), "its window is not memory the kernel allocated"),
            (Some(Err(Lent::Length { is: 2 * WINDOW_BYTES, bytes: WINDOW_BYTES })), "its window is not WINDOW_BYTES long"),
            (Some(Err(Lent::Kernel(SyscallError::InvalidArgument))), "its window would not map"),
            (None, "its hello carried no window"),
        ];
        for (window, why) in refused {
            assert_eq!(hello(0, 0, || window), Hello::Dropped(why));
        }
    }

    /// A stream asked to start past the largest offset a file has is refused,
    /// since its offset would grow by what it drains; and the same bound holds
    /// a write's end and a truncation.
    #[test]
    fn an_offset_past_the_largest_a_file_has_is_refused() {
        for at in [MAX_FILE_BYTES + 1, u64::MAX - 1] {
            assert_eq!(stream(at, 0, 0), Err(Error::FileTooLarge), "a stream at {at:#x}");
        }
        assert_eq!(stream(MAX_FILE_BYTES, 0, 0), Ok(()));
        assert_eq!(in_a_file(MAX_FILE_BYTES - 2, 4), Err(Error::FileTooLarge), "a write straddling the ceiling");
        assert_eq!(in_a_file(u64::MAX, 1), Err(Error::FileTooLarge), "a write whose end overflows");
        assert_eq!(in_a_file(MAX_FILE_BYTES - 4, 4), Ok(MAX_FILE_BYTES));
    }

    #[test]
    fn a_stream_past_the_server_s_bound_or_its_share_s_part_is_refused() {
        assert_eq!(stream(0, MAX_STREAMS, 0), Err(Error::TooManyStreams));
        assert_eq!(stream(0, STREAM_SHARE, STREAM_SHARE), Err(Error::TooManyStreams));
        assert_eq!(stream(0, MAX_STREAMS - 1, STREAM_SHARE - 1), Ok(()));
    }

    /// A path put on the wire past std is held to what std would have sent
    /// before anything is looked up: a `..` that climbs, a `.` and an
    /// absolute path are refused.
    #[test]
    fn a_path_on_the_wire_that_climbs_or_is_absolute_is_refused() {
        for bad in ["../state/secret", "a/../../state/secret", "/state", "./a", "a//b"] {
            assert_eq!(relative(bad), Err(Error::InvalidPath), "{bad:?}");
        }
        assert_eq!(relative("a/b.txt"), Ok(()));
        assert_eq!(relative(""), Ok(()), "the directory itself");
    }

    /// A link's target is the client's text, stored as written: an absolute
    /// target and one that climbs are taken, and only bytes that are no UTF-8
    /// are refused.
    #[test]
    fn a_link_s_target_is_taken_absolute_or_climbing() {
        assert_eq!(path(b"/home/x".to_vec()), Ok("/home/x".to_string()));
        assert_eq!(path(b"../x".to_vec()), Ok("../x".to_string()));
        assert_eq!(path(b"\xff".to_vec()), Err(Error::InvalidPath));
    }

    #[test]
    fn a_path_longer_than_a_path_or_past_the_window_is_refused() {
        assert_eq!(path_span(0, MAX_PATH as u64), Ok((0, MAX_PATH)));
        assert_eq!(path_span(0, MAX_PATH as u64 + 1), Err(Error::NameTooLong));
        assert_eq!(path_span(0, u64::MAX), Err(Error::NameTooLong));
        assert_eq!(path_span((WINDOW_BYTES - 1) as u64, 2), Err(Error::InvalidPath));
        assert_eq!(path_span(u64::MAX, 1), Err(Error::InvalidPath));
    }
}
