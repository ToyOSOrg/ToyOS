//! libc's socket option rule against the host's own `setsockopt` and
//! `getsockopt`, one table through both: `SO_BROADCAST` on a datagram socket
//! and `TCP_NODELAY` on a stream socket, a value and a buffer of 8, 4, 2, 1
//! and 0 bytes and a null one of each length, and a null length. Compared are
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
    })
}

fn host_refusal() -> Answer {
    // SAFETY: the host's errno slot is this thread's.
    Answer::Refused(unsafe { *errno_location() })
}

fn read(buf: &[u8; 8], len: u32) -> Answer {
    let written = buf.map(|byte| byte != UNWRITTEN);
    Answer::Read { len, written, nonzero: buf.iter().any(|&byte| byte != UNWRITTEN && byte != 0) }
}

/// One socket's option as libc keeps it and as the host does.
struct Pair {
    option: Kept,
    fd: c_int,
    kept: bool,
}

impl Pair {
    /// A setter handed `len` bytes at `value`, by libc's rule and by the host.
    fn set(&mut self, value: *const u8, len: u32) -> (Answer, Answer) {
        let (level, name) = libc_numbers(self.option);
        assert_eq!(sockopt::kept(level, name), Some(self.option));
        // SAFETY: `value` is null or eight readable bytes, and `len` at most 8.
        let ours = match unsafe { sockopt::switch(value, len) } {
            Ok(on) => {
                self.kept = on;
                Answer::Set
            }
            Err(refusal) => refused(refusal),
        };
        let (level, name) = host_numbers(self.option);
        // SAFETY: as above.
        let host = match unsafe { setsockopt(self.fd, level, name, value, len) } {
            0 => Answer::Set,
            _ => host_refusal(),
        };
        (ours, host)
    }

    /// A getter handed `len` bytes at `value`, null when `null`, by libc's rule
    /// and by the host.
    fn get(&self, null: bool, len: u32) -> (Answer, Answer) {
        let call = |get: &dyn Fn(*mut u8, *mut u32) -> Option<Answer>| {
            let (mut buf, mut len) = ([UNWRITTEN; 8], len);
            let value = if null { ptr::null_mut() } else { buf.as_mut_ptr() };
            get(value, &mut len).unwrap_or_else(|| read(&buf, len))
        };
        // SAFETY: the buffer is null or eight writable bytes, and `len` at most 8.
        let ours = call(&|value, len| unsafe { sockopt::answer(self.kept as i32, value, len) }.err().map(refused));
        let (level, name) = host_numbers(self.option);
        // SAFETY: as above.
        let host = call(&|value, len| (unsafe { getsockopt(self.fd, level, name, value, len) } != 0).then(host_refusal));
        (ours, host)
    }
}

/// What Darwin answers where it is not what Linux and libc do. It judges a
/// setter's pointer before its length unless the length is 0, and a getter's
/// null buffer holds no byte, which it reports as a read of length 0.
fn darwin(setter: bool, null: bool, len: u32) -> Option<Answer> {
    match (setter, null, len) {
        (true, true, 1 | 2) => Some(Answer::Refused(EFAULT)),
        (false, true, 1..) => Some(Answer::Read { len: 0, written: [false; 8], nonzero: false }),
        _ => None,
    }
}

fn hold((ours, host): (Answer, Answer), setter: bool, null: bool, len: u32, row: &str) {
    let expected = match darwin(setter, null, len) {
        Some(theirs) if cfg!(target_os = "macos") => theirs,
        _ => ours,
    };
    assert_eq!(host, expected, "{row}: libc answers {ours:?}");
}

#[test]
fn an_options_value_crosses_as_the_hosts_does() {
    let datagram = UdpSocket::bind("127.0.0.1:0").unwrap();
    let stream = TcpListener::bind("127.0.0.1:0").unwrap();
    for (option, fd) in [(Kept::Broadcast, datagram.as_raw_fd()), (Kept::NoDelay, stream.as_raw_fd())] {
        let mut pair = Pair { option, fd, kept: false };
        for on in [1u8, 0, 1] {
            let value = [on, 0, 0, 0, 0, 0, 0, 0];
            for len in LENGTHS {
                for null in [false, true] {
                    let row = format!("{option:?} {on}, {len} bytes, null {null}");
                    let at = if null { ptr::null() } else { value.as_ptr() };
                    hold(pair.set(at, len), true, null, len, &format!("set {row}"));
                    hold(pair.get(null, len), false, null, len, &format!("get {row}"));
                }
            }
            assert_eq!(pair.kept, on != 0, "{option:?}: the value of four bytes or more was kept");
            let (ours, host) = pair.get(false, 4);
            assert_eq!(ours, Answer::Read { len: 4, written: [true, true, true, true, false, false, false, false], nonzero: on != 0 });
            assert_eq!(host, ours);
        }

        let (level, name) = host_numbers(option);
        let mut buf = [UNWRITTEN; 8];
        // SAFETY: a null length is the refusal asked for; the buffer is eight bytes.
        let host = unsafe { getsockopt(fd, level, name, buf.as_mut_ptr(), ptr::null_mut()) };
        assert_eq!((host, host_refusal()), (-1, Answer::Refused(EFAULT)), "{option:?}: a null length");
        // SAFETY: as above.
        let ours = unsafe { sockopt::answer(1, buf.as_mut_ptr(), ptr::null_mut()) };
        assert_eq!((ours, buf), (Err(Refusal::Fault), [UNWRITTEN; 8]));
    }
}
