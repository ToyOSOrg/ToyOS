//! libc's IPv4 addresses (`inaddr.rs`) against the host C library's: the
//! texts `inet_pton` and `inet_addr` read, the text `inet_ntop` writes, and a
//! `sockaddr_in`'s bytes against the host's `htonl` and `htons`. Where the
//! hosts' own readers part from POSIX's text or from each other, the text is
//! held alone, each case saying which.

use std::ffi::{c_char, c_int, c_void, CStr, CString};

use crate::inaddr::{dotted_quad, dotted_text, numbers_and_dots, SockaddrIn, AF_INET};

unsafe extern "C" {
    fn inet_pton(af: c_int, src: *const c_char, dst: *mut c_void) -> c_int;
    fn inet_ntop(af: c_int, src: *const c_void, dst: *mut c_char, size: u32) -> *const c_char;
    fn inet_addr(cp: *const c_char) -> u32;
    fn htonl(hostlong: u32) -> u32;
    fn htons(hostshort: u16) -> u16;
}

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

#[test]
fn a_dotted_quad_reads_as_the_host_reads_it() {
    let mut parts = OCTETS.to_vec();
    parts.extend(["256", "300", "999", "1000", "", "a", " ", "1 ", " 1", "-1", "+1", "1x", "0x1"]);
    let mut read = 0;
    let mut judge = |text: &str| {
        let ours = dotted_quad(text.as_bytes());
        assert_eq!(ours, host_pton(text), "inet_pton({text:?})");
        read += usize::from(ours.is_some());
    };
    texts(&parts, 1..=4, &[""], &mut judge);
    texts(&parts[..12], 5..=5, &[""], &mut judge);
    texts(&OCTETS, 4..=4, &[".", " ", "\n", "x"], &mut judge);
    assert_eq!(read, OCTETS.len().pow(4), "the texts that are addresses");
}

/// POSIX: each number is "a one to three-digit decimal number between 0 and
/// 255". glibc refuses a leading zero and Darwin takes any count of digits,
/// so neither is asked.
#[test]
fn a_dotted_quad_s_number_is_one_to_three_decimal_digits() {
    assert_eq!(dotted_quad(b"010.001.00.0"), Some([10, 1, 0, 0]));
    assert_eq!(dotted_quad(b"0001.2.3.4"), None);
    assert_eq!(dotted_quad(b"1.2.3.0255"), None);
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
    judge(" 1.2.3.4");
    judge("1.2.3.4\x0b");
    judge("1.2.3.4\x0c\r");
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

#[test]
fn an_address_is_written_as_the_host_writes_it() {
    for position in 0..4 {
        for octet in 0..=255u8 {
            let mut ip = [7, 77, 177, 0];
            ip[position] = octet;
            let mut ours = [0xaau8; 16];
            let len = dotted_text(ip, &mut ours);
            let mut host = [0 as c_char; 16];
            // SAFETY: four bytes to read and sixteen to write.
            let wrote = unsafe { inet_ntop(AF_INET, ip.as_ptr().cast(), host.as_mut_ptr(), 16) };
            assert!(!wrote.is_null(), "the host's inet_ntop({ip:?})");
            // SAFETY: the host's NUL-terminated answer.
            let want = unsafe { CStr::from_ptr(wrote) }.to_bytes_with_nul();
            assert_eq!(&ours[..=len], want, "{ip:?}");
        }
    }
    let mut longest = [0xaau8; 16];
    assert_eq!(dotted_text([255; 4], &mut longest), 15);
    assert_eq!(&longest, b"255.255.255.255\0");
}

/// `sockaddr_in` as Linux and `include/netinet/in.h` lay it out: the family,
/// then the port's two bytes and the address's four in network order.
#[test]
fn a_sockaddr_holds_its_port_and_address_in_network_order() {
    assert_eq!(size_of::<SockaddrIn>(), 16);
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
