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
//! **An export takes its served slot when it is made**, of the share of the
//! connection that asked, and is refused there: a connection made for a
//! child is served from its export, waits on no handshake bound or timeout,
//! and its hello is not counted again.
//!
//! **Every client number is bounded where it is kept**: an offset a request
//! names, and an offset a stream grows by what it drains, stay at or below the
//! largest a file has ([`MAX_FILE_BYTES`]).
//!
//! **A request reaches only its span of the window**, which lies inside it.
//! **A path is the span's bytes the request names, UTF-8**, and one the
//! server resolves is relative ([`toyos::fs::relative`]): an empty or `.`
//! component is refused before anything is looked up, and a `..` is the
//! resolver's to bound. A link's target is stored as it was written, never
//! resolved here, so it is held to UTF-8 alone.

use toyos::fs::{Error, MAX_FILE_BYTES, MAX_PATH, WINDOW_BYTES};

use crate::descriptions::MAX_IDS;
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

/// Whether a connection's served slot is still to be taken at its hello, or
/// was taken when it was exported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    AtHello,
    Taken,
}

/// The slot a new served connection takes, while `served` clients are served
/// and `of_share` of them are its share's: `Err` is why it is refused.
fn served_slot(served: usize, of_share: usize) -> Result<(), String> {
    if served >= MAX_SERVED {
        return Err(format!("it is refused, since {MAX_SERVED} clients are served already"));
    }
    if of_share >= SERVED_SHARE {
        return Err(format!("it is refused, since its share is served {SERVED_SHARE} clients already"));
    }
    Ok(())
}

/// An export, while `served` clients are served and `of_share` of them are
/// the asking connection's share's: it takes its slot here or is refused
/// `TooManyConnections`.
pub fn export(served: usize, of_share: usize) -> Result<(), Error> {
    served_slot(served, of_share).map_err(|_| Error::TooManyConnections)
}

/// How many ids an export names, read before its window is: 1 to
/// [`MAX_IDS`], what one connection holds, or `InvalidRequest`.
pub fn export_ids(count: u64) -> Result<usize, Error> {
    usize::try_from(count).ok().filter(|n| (1..=MAX_IDS).contains(n)).ok_or(Error::InvalidRequest)
}

/// A hello, while `served` clients are served and `of_share` of them are its
/// share's; `slot` says whether it takes a slot here. `window` is the region
/// it lent as the kernel describes it, `None` for a hello that lent none, and
/// is asked only once the bounds have passed.
pub fn hello<W>(
    slot: Slot,
    served: usize,
    of_share: usize,
    window: impl FnOnce() -> Option<Result<W, Lent>>,
) -> Hello<W> {
    if slot == Slot::AtHello {
        if let Err(why) = served_slot(served, of_share) {
            return Hello::Refused(why);
        }
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

/// Where a seek of `delta` from `base` lands: at or below the largest offset a
/// file has and not before its start, or `InvalidRequest`.
pub fn seek(base: u64, delta: i64) -> Result<u64, Error> {
    base.checked_add_signed(delta).filter(|&at| at <= MAX_FILE_BYTES).ok_or(Error::InvalidRequest)
}

/// A stream, while `streams` are served and `of_share` of them are its share's.
pub fn stream(streams: usize, of_share: usize) -> Result<(), Error> {
    match streams >= MAX_STREAMS || of_share >= STREAM_SHARE {
        true => Err(Error::TooManyStreams),
        false => Ok(()),
    }
}

/// The bytes of the window a request's `at` and `span` name, inside it, or
/// `InvalidRequest`.
pub fn span(at: u64, span: u64) -> Result<(usize, usize), Error> {
    let fits = |at: usize, len: usize| at.checked_add(len).is_some_and(|end| end <= WINDOW_BYTES);
    match (usize::try_from(at), usize::try_from(span)) {
        (Ok(at), Ok(len)) if fits(at, len) => Ok((at, len)),
        _ => Err(Error::InvalidRequest),
    }
}

/// Where in a span of `room` bytes a request's two paths are, `len` bytes
/// from its start and `len2` after them: `NameTooLong` for one longer than a
/// path, `InvalidPath` for two that run past the span.
pub fn paths(room: usize, len: u64, len2: u64) -> Result<(usize, usize), Error> {
    if len > MAX_PATH as u64 || len2 > MAX_PATH as u64 {
        return Err(Error::NameTooLong);
    }
    let (len, len2) = (len as usize, len2 as usize);
    match len + len2 <= room {
        true => Ok((len, len2)),
        false => Err(Error::InvalidPath),
    }
}

/// The path a request's bytes spell: UTF-8, or `InvalidPath`.
pub fn path(bytes: Vec<u8>) -> Result<String, Error> {
    String::from_utf8(bytes).map_err(|_| Error::InvalidPath)
}

/// `rel`, a path the server resolves from a client's directory: relative, or
/// `InvalidPath`.
pub fn relative(rel: &str) -> Result<(), Error> {
    match toyos::fs::relative(rel) {
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
        let at = Slot::AtHello;
        assert!(matches!(hello(at, MAX_SERVED, 0, unasked), Hello::Refused(why) if why.contains("128 clients are served")));
        assert!(matches!(hello(at, SERVED_SHARE, SERVED_SHARE, unasked), Hello::Refused(why) if why.contains("its share is served 32")));
        assert_eq!(hello(at, MAX_SERVED - 1, SERVED_SHARE - 1, || Some(Ok(()))), Hello::Served(()));
    }

    /// An export is refused at the bounds a hello is, before any connection is
    /// made for it.
    #[test]
    fn an_export_past_the_server_s_bound_or_its_share_s_part_is_refused() {
        assert_eq!(export(MAX_SERVED, 0), Err(Error::TooManyConnections));
        assert_eq!(export(SERVED_SHARE, SERVED_SHARE), Err(Error::TooManyConnections));
        assert_eq!(export(MAX_SERVED - 1, SERVED_SHARE - 1), Ok(()));
    }

    /// An export's count is the client's word: none, more than a connection
    /// holds, or one whose window span would overflow is refused before
    /// anything is read.
    #[test]
    fn an_export_naming_no_ids_or_more_than_a_connection_holds_is_refused() {
        for count in [0, MAX_IDS as u64 + 1, 1 << 61, u64::MAX] {
            assert_eq!(export_ids(count), Err(Error::InvalidRequest), "{count:#x}");
        }
        assert_eq!(export_ids(1), Ok(1));
        assert_eq!(export_ids(MAX_IDS as u64), Ok(MAX_IDS));
    }

    /// The exported connection's slot was taken at its export, so its hello is
    /// served at a full server and share, which counted it already; its window
    /// is still held to what a hello's is.
    #[test]
    fn an_exported_connection_s_hello_is_not_counted_again() {
        assert_eq!(hello(Slot::Taken, MAX_SERVED, SERVED_SHARE, || Some(Ok(()))), Hello::Served(()));
        assert_eq!(hello::<()>(Slot::Taken, 0, 0, || None), Hello::Dropped("its hello carried no window"));
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
            assert_eq!(hello(Slot::AtHello, 0, 0, || window), Hello::Dropped(why));
        }
    }

    /// A seek lands at or below the largest offset a file has, and never
    /// before its start; a write's end and a truncation are held to the same.
    #[test]
    fn an_offset_past_the_largest_a_file_has_is_refused() {
        assert_eq!(seek(0, (MAX_FILE_BYTES + 1) as i64), Err(Error::InvalidRequest));
        assert_eq!(seek(MAX_FILE_BYTES, 1), Err(Error::InvalidRequest));
        assert_eq!(seek(3, -4), Err(Error::InvalidRequest), "before the start");
        assert_eq!(seek(u64::MAX, 1), Err(Error::InvalidRequest), "an overflow");
        assert_eq!(seek(10, -4), Ok(6));
        assert_eq!(seek(0, MAX_FILE_BYTES as i64), Ok(MAX_FILE_BYTES));
        assert_eq!(in_a_file(MAX_FILE_BYTES - 2, 4), Err(Error::FileTooLarge), "a write straddling the ceiling");
        assert_eq!(in_a_file(u64::MAX, 1), Err(Error::FileTooLarge), "a write whose end overflows");
        assert_eq!(in_a_file(MAX_FILE_BYTES - 4, 4), Ok(MAX_FILE_BYTES));
    }

    #[test]
    fn a_stream_past_the_server_s_bound_or_its_share_s_part_is_refused() {
        assert_eq!(stream(MAX_STREAMS, 0), Err(Error::TooManyStreams));
        assert_eq!(stream(STREAM_SHARE, STREAM_SHARE), Err(Error::TooManyStreams));
        assert_eq!(stream(MAX_STREAMS - 1, STREAM_SHARE - 1), Ok(()));
    }

    /// A path on the wire that is absolute or has an empty or `.` component
    /// is refused before anything is looked up; a climb is the resolver's.
    #[test]
    fn a_path_on_the_wire_that_is_absolute_or_not_relative_is_refused() {
        for bad in ["/state", "./a", "a//b", "a/", "a/./b"] {
            assert_eq!(relative(bad), Err(Error::InvalidPath), "{bad:?}");
        }
        for good in ["a/b.txt", "", "../x", "a/../b"] {
            assert_eq!(relative(good), Ok(()), "{good:?}");
        }
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

    /// A span is inside the window or refused, at every overflow; its paths
    /// are inside the span and each at most a path long.
    #[test]
    fn a_span_past_the_window_and_a_path_past_its_span_are_refused() {
        assert_eq!(span(0, WINDOW_BYTES as u64), Ok((0, WINDOW_BYTES)));
        assert_eq!(span(WINDOW_BYTES as u64, 0), Ok((WINDOW_BYTES, 0)));
        for (at, len) in [(1, WINDOW_BYTES as u64), (WINDOW_BYTES as u64 + 1, 0), (u64::MAX, 1), (1, u64::MAX)] {
            assert_eq!(span(at, len), Err(Error::InvalidRequest), "{at:#x}+{len:#x}");
        }
        assert_eq!(paths(MAX_PATH * 2, MAX_PATH as u64, MAX_PATH as u64), Ok((MAX_PATH, MAX_PATH)));
        assert_eq!(paths(usize::MAX, MAX_PATH as u64 + 1, 0), Err(Error::NameTooLong));
        assert_eq!(paths(usize::MAX, 0, u64::MAX), Err(Error::NameTooLong));
        assert_eq!(paths(7, 4, 4), Err(Error::InvalidPath));
        assert_eq!(paths(8, 4, 4), Ok((4, 4)));
    }
}
