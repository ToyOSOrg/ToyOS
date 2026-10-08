//! IPv4 addresses and ports as a C program holds them, which is network byte
//! order: an address is its four octets in memory order and a port its two,
//! high first. Every address libc reads from or writes to a C program passes
//! through [`SockaddrIn`], which holds both as bytes, so no integer of either
//! exists to be read in the machine's order. The texts `inet_pton`,
//! `inet_addr` and `inet_ntop` read and write are here too. It reads and sets
//! nothing but what it is handed, so the host tests it (`toyos-libc-copies`).

pub(crate) const AF_INET: i32 = 2;

/// C's `struct sockaddr_in`, as `include/netinet/in.h` lays it out.
#[repr(C)]
pub(crate) struct SockaddrIn {
    sin_family: u16,
    sin_port: [u8; 2],
    sin_addr: [u8; 4],
    sin_zero: [u8; 8],
}

impl SockaddrIn {
    pub(crate) fn new(ip: [u8; 4], port: u16) -> Self {
        Self { sin_family: AF_INET as u16, sin_port: port.to_be_bytes(), sin_addr: ip, sin_zero: [0; 8] }
    }

    /// The address and port, or `None` for another family's.
    pub(crate) fn endpoint(&self) -> Option<([u8; 4], u16)> {
        (self.sin_family == AF_INET as u16).then(|| (self.sin_addr, u16::from_be_bytes(self.sin_port)))
    }
}

/// `inet_pton`'s text, POSIX's `ddd.ddd.ddd.ddd`: four decimal numbers of one
/// to three digits, each at most 255, and nothing else.
pub(crate) fn dotted_quad(text: &[u8]) -> Option<[u8; 4]> {
    let mut octets = [0u8; 4];
    let mut parts = text.split(|&c| c == b'.');
    for octet in &mut octets {
        let part = parts.next()?;
        if !(1..=3).contains(&part.len()) || !part.iter().all(u8::is_ascii_digit) {
            return None;
        }
        let value = part.iter().fold(0u16, |v, &c| v * 10 + u16::from(c - b'0'));
        *octet = u8::try_from(value).ok()?;
    }
    parts.next().is_none().then_some(octets)
}

/// One number of `inet_addr`'s text, as ISO C writes an integer constant:
/// `0x` and hexadecimal digits, `0` and octal digits, or decimal digits.
fn number(part: &[u8]) -> Option<u32> {
    let (digits, radix) = match part {
        [b'0', b'x' | b'X', rest @ ..] => (rest, 16),
        [b'0', rest @ ..] if !rest.is_empty() => (rest, 8),
        _ => (part, 10),
    };
    if digits.is_empty() {
        return None;
    }
    digits.iter().try_fold(0u32, |v, &c| v.checked_mul(radix)?.checked_add(char::from(c).to_digit(radix)?))
}

/// `inet_addr`'s text: one to four numbers between dots, each but the last
/// one octet and the last every octet that is left, so `127.1` is 127.0.0.1.
/// The address ends at the string's end or at white space, as BSD's reader
/// and glibc's have it; POSIX names no end.
pub(crate) fn numbers_and_dots(text: &[u8]) -> Option<[u8; 4]> {
    let end = text.iter().position(|c| matches!(c, b' ' | b'\t'..=b'\r')).unwrap_or(text.len());
    let mut numbers = [0u32; 4];
    let mut count = 0;
    for part in text[..end].split(|&c| c == b'.') {
        *numbers.get_mut(count)? = number(part)?;
        count += 1;
    }
    let (&last, leading) = numbers[..count].split_last()?;
    let mut octets = [0u8; 4];
    for (octet, &n) in octets.iter_mut().zip(leading) {
        *octet = u8::try_from(n).ok()?;
    }
    let left = 4 - leading.len();
    if u64::from(last) >> (8 * left) != 0 {
        return None;
    }
    octets[leading.len()..].copy_from_slice(&last.to_be_bytes()[leading.len()..]);
    Some(octets)
}

/// `ip` as `inet_ntop` writes it into `buf`, NUL-terminated; the text's
/// length without the NUL. `buf` is `INET_ADDRSTRLEN` bytes, which holds the
/// longest.
pub(crate) fn dotted_text(ip: [u8; 4], buf: &mut [u8; 16]) -> usize {
    let mut at = 0;
    for (i, octet) in ip.into_iter().enumerate() {
        if i > 0 {
            buf[at] = b'.';
            at += 1;
        }
        let digits = [octet / 100, octet / 10 % 10, octet % 10];
        let skip = if octet >= 100 { 0 } else if octet >= 10 { 1 } else { 2 };
        for digit in &digits[skip..] {
            buf[at] = b'0' + digit;
            at += 1;
        }
    }
    buf[at] = 0;
    at
}
