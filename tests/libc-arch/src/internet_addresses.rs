//! libc's IPv4 addresses (`inaddr.rs`) against the host C library's: the
//! texts `inet_pton` and `inet_addr` read, the text `inet_ntop` writes into a
//! buffer of every size, both calls' refusal of another family, a
//! `sockaddr_in`'s bytes against the host's `htonl` and `htons`, and the
//! bytes a buffer of every length is answered against the host's
//! `getsockname`. Where the hosts' own readers part from POSIX's text or from
//! each other, each case says which.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::net::UdpSocket;
use std::os::fd::AsRawFd;
use std::ptr;

use crate::inaddr::{ntop, numbers_and_dots, pton, Refusal, SockaddrIn, AF_INET};
use crate::socket_options::host_errno;

unsafe extern "C" {
    fn inet_pton(af: c_int, src: *const c_char, dst: *mut c_void) -> c_int;
    fn inet_ntop(af: c_int, src: *const c_void, dst: *mut c_char, size: u32) -> *const c_char;
    fn inet_addr(cp: *const c_char) -> u32;
    fn htonl(hostlong: u32) -> u32;
    fn htons(hostshort: u16) -> u16;
    fn getsockname(fd: c_int, addr: *mut u8, len: *mut u32) -> c_int;
}

/// The host's `EAFNOSUPPORT`; `ENOSPC` macOS and Linux number alike.
const EAFNOSUPPORT: c_int = if cfg!(target_os = "macos") { 47 } else { 97 };
const ENOSPC: c_int = 28;
/// What a buffer holds before a call writes it.
const UNWRITTEN: u8 = 0xAA;

/// Every text of `min..=max` of `parts` between dots, each followed by each
/// of `ends`.
fn texts(parts: &[&str], counts: std::ops::RangeInclusive<usize>, ends: &[&str], mut each: impl FnMut(&str)) {
    for count in counts {
        for pick in 0..parts.len().pow(count as u32) {
            let chosen: Vec<&str> = (0..count).map(|i| parts[pick / parts.len().pow(i as u32) % parts.len()]).collect();
            for end in ends {
                each(&format!("{}{end}", chosen.join(".")));
            }
        }
    }
}

/// libc's `inet_pton` of an `AF_INET` text.
fn ours_pton(text: &str) -> Option<[u8; 4]> {
    pton(AF_INET, text.as_bytes()).expect("AF_INET is libc's family")
}

/// The host's `inet_pton` of `text`.
fn host_pton(text: &str) -> Option<[u8; 4]> {
    let c = CString::new(text).unwrap();
    let mut octets = [0u8; 4];
    // SAFETY: a NUL-terminated string, and four bytes to write.
    let answer = unsafe { inet_pton(AF_INET, c.as_ptr(), octets.as_mut_ptr().cast()) };
    assert!(answer == 0 || answer == 1, "the host's inet_pton({text:?}) answered {answer}");
    (answer == 1).then_some(octets)
}

/// The host's `inet_addr` of `text`: its answer's bytes in memory order.
fn host_addr(text: &str) -> [u8; 4] {
    let c = CString::new(text).unwrap();
    // SAFETY: a NUL-terminated string.
    unsafe { inet_addr(c.as_ptr()) }.to_ne_bytes()
}

/// libc's `inet_addr`, which answers `INADDR_NONE` for no address.
fn ours_addr(text: &str) -> [u8; 4] {
    numbers_and_dots(text.as_bytes()).unwrap_or([0xff; 4])
}

const OCTETS: [&str; 10] = ["0", "1", "9", "10", "99", "100", "199", "200", "249", "255"];

/// What Darwin's `inet_pton` reads: a number of any count of digits, zeros in
/// front among them, where libc and glibc refuse a zero before another digit
/// and a fourth digit.
fn darwin_reads(text: &str) -> Option<[u8; 4]> {
    let parts: Vec<&str> = text
        .split('.')
        .map(|part| match part.trim_start_matches('0') {
            _ if part.is_empty() || !part.bytes().all(|c| c.is_ascii_digit()) => part,
            "" => "0",
            rest => rest,
        })
        .collect();
    ours_pton(&parts.join("."))
}

#[test]
fn a_dotted_quad_reads_as_the_host_reads_it() {
    let mut parts = OCTETS.to_vec();
    parts.extend(["256", "300", "999", "1000", "", "a", " ", "1 ", " 1", "-1", "+1", "1x", "0x1"]);
    parts.extend(["00", "01", "010", "0255", "0001"]);
    let (mut read, mut zero_led) = (0, 0);
    let mut judge = |text: &str| {
        let ours = ours_pton(text);
        let darwin = darwin_reads(text);
        let want = if cfg!(target_os = "macos") { darwin } else { ours };
        assert_eq!(host_pton(text), want, "inet_pton({text:?}): libc reads {ours:?}");
        read += usize::from(ours.is_some());
        zero_led += usize::from(ours != darwin);
    };
    texts(&parts, 1..=4, &[""], &mut judge);
    texts(&parts[..12], 5..=5, &[""], &mut judge);
    texts(&OCTETS, 4..=4, &[".", " ", "\n", "x"], &mut judge);
    assert_eq!(read, OCTETS.len().pow(4), "the texts that are addresses");
    assert!(zero_led > 10_000, "{zero_led} texts were addresses but for a zero in front");
}

/// No text has two values: `inet_addr` reads `010` as octal, so `inet_pton`
/// refuses it.
#[test]
fn a_number_with_a_zero_before_another_digit_is_no_dotted_quad() {
    assert_eq!(ours_pton("010.0.0.1"), None);
    assert_eq!(numbers_and_dots(b"010.0.0.1"), Some([8, 0, 0, 1]));
    assert_eq!(ours_pton("10.0.0.00"), None);
    assert_eq!(ours_pton("10.0.0.0"), Some([10, 0, 0, 0]));
}

/// A family neither libc nor the host has an address text for. The host's
/// own second family, IPv6, libc refuses the same way and is not asked.
#[test]
fn another_family_is_refused_as_the_host_refuses_it() {
    for af in [0, 1, -1, 12345] {
        assert_eq!(pton(af, b"192.0.2.1"), Err(Refusal::Family), "inet_pton, family {af}");
        let mut octets = [UNWRITTEN; 4];
        // SAFETY: a NUL-terminated string, and four bytes to write.
        let host = unsafe { inet_pton(af, c"192.0.2.1".as_ptr(), octets.as_mut_ptr().cast()) };
        assert_eq!((host, host_errno(), octets), (-1, EAFNOSUPPORT, [UNWRITTEN; 4]), "the host's inet_pton, family {af}");

        let (ip, mut ours, mut theirs) = ([192u8, 0, 2, 1], [UNWRITTEN; 16], [UNWRITTEN; 16]);
        // SAFETY: four bytes to read and sixteen to write.
        let answer = unsafe { ntop(af, ip.as_ptr(), ours.as_mut_ptr(), 16) };
        assert_eq!(answer, Err(Refusal::Family), "inet_ntop, family {af}");
        // SAFETY: as above.
        let host = unsafe { inet_ntop(af, ip.as_ptr().cast(), theirs.as_mut_ptr().cast(), 16) };
        assert_eq!((host, host_errno()), (ptr::null(), EAFNOSUPPORT), "the host's inet_ntop, family {af}");
        assert_eq!(ours, theirs, "family {af}: nothing is written");
    }
}

#[test]
fn numbers_and_dots_read_as_the_host_reads_them() {
    let parts = [
        "0", "1", "9", "255", "256", "65535", "65536", "16777215", "16777216", "4294967295", "0x0", "0xff", "0x100",
        "0xFFFF", "0X10000", "0xffffff", "0x1000000", "0xffffffff", "0xg", "010", "0377", "0400", "08", "00", "", "a",
        "-1", "+1", "1x",
    ];
    let mut read = 0;
    let mut judge = |text: &str| {
        assert_eq!(ours_addr(text), host_addr(text), "inet_addr({text:?})");
        read += usize::from(numbers_and_dots(text.as_bytes()).is_some());
    };
    texts(&parts, 1..=3, &["", " ", "\n", "\t9", " x", "x", "."], &mut judge);
    texts(&parts, 4..=4, &["", " x"], &mut judge);
    texts(&parts[..6], 5..=5, &[""], &mut judge);
    judge(" 192.0.2.1");
    judge("192.0.2.1\x0b");
    judge("192.0.2.1\x0c\r");
    assert!(read > 10_000, "{read} texts were addresses");
}

/// Where BSD's reader parts from POSIX's text, "a 32-bit value" in ISO C's
/// integer forms: it wraps a number past 32 bits and takes `0x` with no
/// digit for zero.
#[test]
fn a_number_past_32_bits_or_without_digits_is_no_address() {
    assert_eq!(numbers_and_dots(b"4294967296"), None);
    assert_eq!(numbers_and_dots(b"0x100000000"), None);
    assert_eq!(numbers_and_dots(b"0x.1"), None);
    assert_eq!(numbers_and_dots(b"1.0x"), None);
}

/// `inet_ntop` of `ip` into `size` bytes of a buffer, by libc and by the
/// host: libc's answer and its buffer, then the host's `errno` where it
/// answered none and its buffer.
fn written(ip: [u8; 4], size: u32) -> (Result<(), Refusal>, [u8; 32], Option<c_int>, [u8; 32]) {
    let (mut ours, mut theirs) = ([UNWRITTEN; 32], [UNWRITTEN; 32]);
    // SAFETY: four bytes to read, and `size`, at most 32, to write.
    let answer = unsafe { ntop(AF_INET, ip.as_ptr(), ours.as_mut_ptr(), size) };
    // SAFETY: as above.
    let host = unsafe { inet_ntop(AF_INET, ip.as_ptr().cast(), theirs.as_mut_ptr().cast(), size) };
    assert!(host.is_null() || host == theirs.as_ptr().cast(), "the host answers its buffer");
    (answer, ours, host.is_null().then(host_errno), theirs)
}

#[test]
fn an_address_is_written_as_the_host_writes_it() {
    for position in 0..4 {
        for octet in 0..=255u8 {
            let mut ip = [7, 77, 177, 0];
            ip[position] = octet;
            let (answer, ours, refused, host) = written(ip, 16);
            assert_eq!((answer, refused), (Ok(()), None), "{ip:?}");
            assert_eq!(ours, host, "{ip:?}");
            assert!(CStr::from_bytes_until_nul(&ours).is_ok(), "{ip:?}: the text ends in a NUL");
        }
    }
}

/// One address for each length a text has, 7 to 15, into a buffer of every
/// size from none to one past `INET_ADDRSTRLEN`: the text fits where its NUL
/// does, and a buffer it does not fit is left as it was.
#[test]
fn a_buffer_too_short_for_the_text_is_refused_as_the_host_refuses_it() {
    let addresses: [[u8; 4]; 9] = [
        [0, 0, 0, 0],
        [10, 0, 0, 0],
        [10, 10, 0, 0],
        [10, 10, 10, 0],
        [10, 10, 10, 10],
        [100, 10, 10, 10],
        [100, 100, 10, 10],
        [100, 100, 100, 10],
        [255, 255, 255, 255],
    ];
    for (ip, text_len) in addresses.into_iter().zip(7u32..) {
        for size in 0..=17 {
            let (answer, ours, refused, host) = written(ip, size);
            let fits = size > text_len;
            assert_eq!(answer, if fits { Ok(()) } else { Err(Refusal::Room) }, "{ip:?} into {size}");
            assert_eq!(refused, (!fits).then_some(ENOSPC), "the host's inet_ntop of {ip:?} into {size}");
            assert_eq!(ours, host, "{ip:?} into {size}");
            assert!(ours[size as usize..].iter().all(|&byte| byte == UNWRITTEN), "{ip:?}: a byte past {size}");
        }
    }
}

/// `sockaddr_in` as Linux and `include/netinet/in.h` lay it out: the family,
/// then the port's two bytes and the address's four in network order.
#[test]
fn a_sockaddr_holds_its_port_and_address_in_network_order() {
    assert_eq!((size_of::<SockaddrIn>(), align_of::<SockaddrIn>()), (16, 4));
    // SAFETY: sixteen bytes with no padding, read as bytes.
    let bytes: [u8; 16] = unsafe { std::mem::transmute(SockaddrIn::new([10, 0, 2, 2], 0x1234)) };
    let family = (AF_INET as u16).to_ne_bytes();
    assert_eq!(bytes, [family[0], family[1], 0x12, 0x34, 10, 0, 2, 2, 0, 0, 0, 0, 0, 0, 0, 0]);

    // What a C program writes: `sin_port = htons(port)` and
    // `sin_addr.s_addr = htonl(address)`, each stored as the machine stores it.
    let mut written = [0u8; 16];
    written[..2].copy_from_slice(&family);
    // SAFETY: the host's byte swaps.
    let (port, address) = unsafe { (htons(0x1234), htonl(0x0a00_0202)) };
    written[2..4].copy_from_slice(&port.to_ne_bytes());
    written[4..8].copy_from_slice(&address.to_ne_bytes());
    // SAFETY: every sixteen bytes are a `SockaddrIn`.
    let read: SockaddrIn = unsafe { std::mem::transmute(written) };
    assert_eq!(read.endpoint(), Some(([10, 0, 2, 2], 0x1234)));

    written[..2].copy_from_slice(&1u16.to_ne_bytes());
    // SAFETY: as above.
    let unix: SockaddrIn = unsafe { std::mem::transmute(written) };
    assert_eq!(unix.endpoint(), None, "AF_UNIX's is no IPv4 address");
}

/// A bound socket's address into a buffer of every length to twice its own,
/// by `SockaddrIn::answer` and by the host's `getsockname`: no byte past the
/// length handed in, and the address's own length written back. The first two
/// bytes are compared on Linux, whose layout libc's is; Darwin's are a length
/// and a one-byte family.
#[test]
fn an_address_is_truncated_to_the_callers_buffer_as_the_host_truncates_it() {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let address = SockaddrIn::new([127, 0, 0, 1], socket.local_addr().unwrap().port());
    for handed in 0..=32u32 {
        let (mut ours, mut theirs) = ([UNWRITTEN; 32], [UNWRITTEN; 32]);
        let (mut our_len, mut their_len) = (handed, handed);
        // SAFETY: a length, and that many of the buffer's 32 bytes.
        unsafe { address.answer(ours.as_mut_ptr(), &mut our_len) };
        // SAFETY: as above.
        let host = unsafe { getsockname(socket.as_raw_fd(), theirs.as_mut_ptr(), &mut their_len) };
        assert_eq!((host, their_len), (0, our_len), "getsockname into {handed}");
        assert_eq!(our_len, 16, "into {handed}: the address's own length");
        let layout = if cfg!(target_os = "linux") { 0 } else { 2 };
        assert_eq!(ours[layout..], theirs[layout..], "into {handed}");
        let kept = (handed as usize).min(16);
        assert!(ours[kept..].iter().all(|&byte| byte == UNWRITTEN), "into {handed}: a byte past the buffer");
        assert!(theirs[kept..].iter().all(|&byte| byte == UNWRITTEN), "the host, into {handed}");
    }
}
