//! libc's socket option rule against the host's own `setsockopt` and
//! `getsockopt`, one table through both: `SO_BROADCAST` on a datagram socket,
//! `TCP_NODELAY` on a stream socket and `TCP_NODELAY` on a datagram socket,
//! which has no such option, a value and a buffer of 8, 4, 2, 1 and 0 bytes
//! and a null one of each length, and a null length. Compared are
//! the return and its `errno`, the length written back, which bytes of the
//! buffer were written, and whether what they hold is non-zero: a host reads a
//! set option back as any non-zero `int`, Darwin as the option's flag bit.
//!
//! libc's numbers and answers are Linux's, so on Linux every row agrees. Darwin
//! answers the rows [`darwin`] names differently, and is held to those.

use std::ffi::c_int;
use std::net::{TcpListener, UdpSocket};
use std::os::fd::AsRawFd;
use std::ptr;

use crate::header;
use crate::sockopt::{self, Kept, Refusal};

const SOCKET_H: &str = include_str!("../../../userland/libc/include/sys/socket.h");
const TCP_H: &str = include_str!("../../../userland/libc/include/netinet/tcp.h");

unsafe extern "C" {
    fn setsockopt(fd: c_int, level: c_int, name: c_int, value: *const u8, len: u32) -> c_int;
    fn getsockopt(fd: c_int, level: c_int, name: c_int, value: *mut u8, len: *mut u32) -> c_int;
    #[cfg_attr(target_os = "macos", link_name = "__error")]
    #[cfg_attr(target_os = "linux", link_name = "__errno_location")]
    fn errno_location() -> *mut c_int;
}

/// The host's `EFAULT` and `EINVAL`, which macOS and Linux number alike.
const EFAULT: c_int = 14;
const EINVAL: c_int = 22;
/// Linux's `ENOPROTOOPT` and `EOPNOTSUPP`: Darwin answers neither here.
const ENOPROTOOPT: c_int = 92;
const EOPNOTSUPP: c_int = 95;

const LENGTHS: [u32; 5] = [8, 4, 2, 1, 0];
/// What a buffer holds before a read, which no answer's byte is.
const UNWRITTEN: u8 = 0xAA;

/// The option as a C program built against libc's headers names it.
fn libc_numbers(option: Kept) -> (c_int, c_int) {
    match option {
        Kept::Broadcast => (header::int(SOCKET_H, "SOL_SOCKET"), header::int(SOCKET_H, "SO_BROADCAST")),
        Kept::NoDelay => (header::int(SOCKET_H, "IPPROTO_TCP"), header::int(TCP_H, "TCP_NODELAY")),
    }
}

/// The option as the host numbers it: on Linux, as libc does.
fn host_numbers(option: Kept) -> (c_int, c_int) {
    if cfg!(target_os = "macos") {
        match option {
            Kept::Broadcast => (0xFFFF, 0x20),
            Kept::NoDelay => (6, 1),
        }
    } else {
        libc_numbers(option)
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Answer {
    /// -1, and this `errno`. A refused read's length is not compared: POSIX
    /// leaves it unspecified.
    Refused(c_int),
    /// A setter's 0.
    Set,
    /// A getter's 0: the length written back, which bytes were written, and
    /// whether they hold a non-zero value.
    Read { len: u32, written: [bool; 8], nonzero: bool },
}

fn refused(refusal: Refusal) -> Answer {
    Answer::Refused(match refusal {
        Refusal::Short => EINVAL,
        Refusal::Fault => EFAULT,
        Refusal::NoSuchOption => ENOPROTOOPT,
        Refusal::NotSupported => EOPNOTSUPP,
    })
}

/// The host's `errno`, as its last call left it.
pub(crate) fn host_errno() -> c_int {
    // SAFETY: the host's errno slot is this thread's.
    unsafe { *errno_location() }
}

fn read(buf: &[u8; 8], len: u32) -> Answer {
    let written = buf.map(|byte| byte != UNWRITTEN);
    Answer::Read { len, written, nonzero: buf.iter().any(|&byte| byte != UNWRITTEN && byte != 0) }
}

/// One socket's option as libc keeps it and as the host does.
struct Pair {
    option: Kept,
    fd: c_int,
    datagram: bool,
    kept: bool,
}

impl Pair {
    /// A TCP option asked of a datagram socket, which has none.
    fn foreign(&self) -> bool {
        self.datagram && self.option == Kept::NoDelay
    }

    /// The option by libc's rule, asked of this socket by a setter or a getter.
    fn named(&self, setter: bool) -> Result<(), Answer> {
        let (level, name) = libc_numbers(self.option);
        let named = sockopt::kept(level, name, self.datagram, setter).map_err(refused)?;
        assert_eq!(named, Some(self.option));
        Ok(())
    }

    /// A setter handed `len` bytes at `value`, by libc's rule and by the host.
    fn set(&mut self, value: *const u8, len: u32) -> (Answer, Answer) {
        // SAFETY: `value` is null or eight readable bytes, and `len` at most 8.
        let ours = match self.named(true).map(|()| unsafe { sockopt::switch(value, len) }) {
            Ok(Ok(on)) => {
                self.kept = on;
                Answer::Set
            }
            Ok(Err(refusal)) => refused(refusal),
            Err(refused) => refused,
        };
        let (level, name) = host_numbers(self.option);
        // SAFETY: as above.
        let host = match unsafe { setsockopt(self.fd, level, name, value, len) } {
            0 => Answer::Set,
            _ => Answer::Refused(host_errno()),
        };
        (ours, host)
    }

    /// A getter handed `value` and `len`, by libc's rule and by the host.
    fn answered(&self, value: *mut u8, len: *mut u32) -> (Option<Answer>, Option<Answer>) {
        // SAFETY: the caller's.
        let ours = self.named(false).and_then(|()| unsafe { sockopt::answer(self.kept as i32, value, len) }.map_err(refused));
        let (level, name) = host_numbers(self.option);
        // SAFETY: the caller's.
        let host = (unsafe { getsockopt(self.fd, level, name, value, len) } != 0).then(|| Answer::Refused(host_errno()));
        (ours.err(), host)
    }

    /// A getter handed `len` bytes at a buffer, null when `null`, by libc's
    /// rule and by the host.
    fn get(&self, null: bool, len: u32) -> (Answer, Answer) {
        let call = |ours: bool| {
            let (mut buf, mut len) = ([UNWRITTEN; 8], len);
            let value = if null { ptr::null_mut() } else { buf.as_mut_ptr() };
            // The buffer is null or eight writable bytes, and `len` at most 8.
            let (our_refusal, host_refusal) = self.answered(value, &mut len);
            (if ours { our_refusal } else { host_refusal }).unwrap_or_else(|| read(&buf, len))
        };
        (call(true), call(false))
    }
}

/// What Darwin answers where it is not what Linux and libc do. A TCP option
/// asked of a datagram socket it calls invalid, once it has read a setter's
/// value. Of an option the socket has, it judges a setter's pointer before its
/// length unless the length is 0, and a getter's null buffer holds no byte,
/// which it reports as a read of length 0.
fn darwin(foreign: bool, setter: bool, null: bool, len: u32) -> Option<Answer> {
    match (foreign, setter, null, len) {
        (true, true, true, 1..) => Some(Answer::Refused(EFAULT)),
        (true, ..) => Some(Answer::Refused(EINVAL)),
        (false, true, true, 1 | 2) => Some(Answer::Refused(EFAULT)),
        (false, false, true, 1..) => Some(Answer::Read { len: 0, written: [false; 8], nonzero: false }),
        _ => None,
    }
}

fn hold((ours, host): (Answer, Answer), theirs: Option<Answer>, row: &str) {
    let expected = match theirs {
        Some(theirs) if cfg!(target_os = "macos") => theirs,
        _ => ours,
    };
    assert_eq!(host, expected, "{row}: libc answers {ours:?}");
}

#[test]
fn an_options_value_crosses_as_the_hosts_does() {
    let datagram = UdpSocket::bind("127.0.0.1:0").unwrap();
    let stream = TcpListener::bind("127.0.0.1:0").unwrap();
    let rows = [
        (Kept::Broadcast, datagram.as_raw_fd(), true),
        (Kept::NoDelay, stream.as_raw_fd(), false),
        (Kept::NoDelay, datagram.as_raw_fd(), true),
    ];
    for (option, fd, is_datagram) in rows {
        let mut pair = Pair { option, fd, datagram: is_datagram, kept: false };
        let foreign = pair.foreign();
        let of = if is_datagram { "a datagram socket" } else { "a stream" };
        for on in [1u8, 0, 1] {
            let value = [on, 0, 0, 0, 0, 0, 0, 0];
            for len in LENGTHS {
                for null in [false, true] {
                    let row = format!("{option:?} of {of} {on}, {len} bytes, null {null}");
                    let at = if null { ptr::null() } else { value.as_ptr() };
                    let set = pair.set(at, len);
                    hold(set, darwin(foreign, true, null, len), &format!("set {row}"));
                    let got = pair.get(null, len);
                    hold(got, darwin(foreign, false, null, len), &format!("get {row}"));
                    if foreign {
                        assert_eq!(set.0, refused(Refusal::NoSuchOption), "set {row}");
                        assert_eq!(got.0, refused(Refusal::NotSupported), "get {row}");
                    }
                }
            }
            if foreign {
                continue;
            }
            assert_eq!(pair.kept, on != 0, "{option:?}: the value of four bytes or more was kept");
            let (ours, host) = pair.get(false, 4);
            assert_eq!(ours, Answer::Read { len: 4, written: [true, true, true, true, false, false, false, false], nonzero: on != 0 });
            assert_eq!(host, ours);
        }

        let mut buf = [UNWRITTEN; 8];
        // SAFETY: a null length is the refusal asked for; the buffer is eight bytes.
        let (ours, host) = pair.answered(buf.as_mut_ptr(), ptr::null_mut());
        let want = if foreign { Refusal::NotSupported } else { Refusal::Fault };
        assert_eq!((ours, buf), (Some(refused(want)), [UNWRITTEN; 8]), "{option:?} of {of}: a null length");
        // Darwin reads the length first, whichever option is asked.
        let theirs = foreign.then_some(Answer::Refused(EFAULT));
        hold((refused(want), host.expect("the host refuses a null length")), theirs, &format!("{option:?} of {of}: a null length"));
    }
}
