//! IPv4 addresses and ports as a C program holds them, which is network byte
//! order: an address is its four octets in memory order and a port its two,
//! high first. Every address libc reads from or writes to a C program passes
//! through [`SockaddrIn`], which holds both as bytes, so no integer of either
//! exists to be read in the machine's order, and which answers a caller's
//! buffer no more bytes than it holds. The texts `inet_pton`, `inet_addr` and
//! `inet_ntop` read and write are here too, with those calls' refusals. It
//! reads and sets nothing but what it is handed, so the host tests it
//! (`toyos-libc-copies`) against the host C library's own calls.

use core::mem::size_of;

pub(crate) const AF_INET: i32 = 2;

/// C's `struct sockaddr_in`, as `include/netinet/in.h` lays it out.
#[repr(C, align(4))]
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

    /// Answers a caller this address as POSIX has a call store one: as many
    /// of its bytes as `*len` holds, and its own length written back.
    ///
    /// # Safety
    /// `len` is a readable and writable length, and `addr` `*len` writable
    /// bytes.
    pub(crate) unsafe fn answer(&self, addr: *mut u8, len: *mut u32) {
        let whole = size_of::<Self>();
        core::ptr::copy_nonoverlapping((self as *const Self).cast::<u8>(), addr, whole.min(*len as usize));
        *len = whole as u32;
    }
}

/// Why `inet_pton` or `inet_ntop` answers no address.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Refusal {
    /// A family that is not `AF_INET`: `EAFNOSUPPORT`.
    Family,
    /// `inet_ntop`'s buffer does not hold the text and its NUL: `ENOSPC`.
    Room,
}

/// `inet_pton`: `text`'s address, `None` for a text that is none.
pub(crate) fn pton(af: i32, text: &[u8]) -> Result<Option<[u8; 4]>, Refusal> {
    if af != AF_INET {
        return Err(Refusal::Family);
    }
    Ok(dotted_quad(text))
}

/// `inet_ntop`: the text of the address at `src` and its NUL into the `size`
/// bytes at `dst`, which a refusal leaves unwritten.
///
/// # Safety
/// `src` is an `AF_INET` address's four bytes, read once the family is known
/// to be that, and `dst` is `size` writable bytes.
pub(crate) unsafe fn ntop(af: i32, src: *const u8, dst: *mut u8, size: u32) -> Result<(), Refusal> {
    if af != AF_INET {
        return Err(Refusal::Family);
    }
    let mut text = [0u8; 16];
    let len = dotted_text(src.cast::<[u8; 4]>().read(), &mut text);
    if len as u32 >= size {
        return Err(Refusal::Room);
    }
    core::ptr::copy_nonoverlapping(text.as_ptr(), dst, len + 1);
    Ok(())
}

/// `inet_pton`'s text, POSIX's `ddd.ddd.ddd.ddd`: four decimal numbers of one
/// to three digits, each at most 255, and nothing else. A number of more than
/// one digit that begins with `0` is refused, as glibc refuses it: `inet_addr`
/// reads such a number as octal, and no text has two values.
pub(crate) fn dotted_quad(text: &[u8]) -> Option<[u8; 4]> {
    let mut octets = [0u8; 4];
    let mut parts = text.split(|&c| c == b'.');
    for octet in &mut octets {
        let part = parts.next()?;
        let zero_led = part.len() > 1 && part[0] == b'0';
        if !(1..=3).contains(&part.len()) || zero_led || !part.iter().all(u8::is_ascii_digit) {
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
fn dotted_text(ip: [u8; 4], buf: &mut [u8; 16]) -> usize {
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
