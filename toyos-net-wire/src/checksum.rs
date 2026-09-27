//! The Internet checksum (RFC 1071) and the IPv4 pseudo-header; a received checksum verifies when its covered sum is 0xFFFF.

use core::net::Ipv4Addr;

use crate::emit::BuildError;
use crate::ipv4::Protocol;

fn add16(a: u16, b: u16) -> u16 {
    let (sum, carry) = a.overflowing_add(b);
    // A carry leaves `sum` at most 0xFFFE, so adding it back cannot carry.
    sum.wrapping_add(u16::from(carry))
}

/// 2^64 ≡ 1 (mod 0xFFFF), so a 64-bit end-around carry is the 16-bit sum's.
fn add64(a: u64, b: u64) -> u64 {
    let (sum, carry) = a.overflowing_add(b);
    sum.wrapping_add(u64::from(carry))
}

/// Two carry chains, so neither waits on the other.
fn add_words(sum: u64, bytes: &[u8]) -> (u64, Option<u8>) {
    let (words, tail) = bytes.as_chunks::<8>();
    let (pairs, last) = words.as_chunks::<2>();
    let (mut a, mut b) = (sum, 0);
    for [x, y] in pairs {
        a = add64(a, u64::from_ne_bytes(*x));
        b = add64(b, u64::from_ne_bytes(*y));
    }
    for word in last {
        a = add64(a, u64::from_ne_bytes(*word));
    }
    let (pairs, odd) = tail.as_chunks::<2>();
    for pair in pairs {
        a = add64(a, u64::from(u16::from_ne_bytes(*pair)));
    }
    (add64(a, b), odd.first().copied())
}

/// Words are summed in native byte order and swapped once, in `sum` (RFC 1071 §2(B)).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Accumulator {
    sum: u64,
    pending: Option<u8>,
}

impl Accumulator {
    pub const fn new() -> Self {
        Self { sum: 0, pending: None }
    }

    #[must_use]
    pub fn feed(self, bytes: &[u8]) -> Self {
        let (sum, rest) = match (self.pending, bytes.split_first()) {
            (Some(high), Some((&low, rest))) => (add64(self.sum, u64::from(u16::from_ne_bytes([high, low]))), rest),
            (Some(_), None) => return self,
            (None, _) => (self.sum, bytes),
        };
        let (sum, pending) = add_words(sum, rest);
        Self { sum, pending }
    }

    /// Copies `from` to the front of `to` and sums it, a block at a time, so the bytes are read once.
    pub(crate) fn copy(self, to: &mut [u8], from: &[u8]) -> Result<Self, BuildError> {
        let (to, _) = to.split_at_mut_checked(from.len()).ok_or(BuildError::BufferTooSmall)?;
        Ok(to.chunks_mut(512).zip(from.chunks(512)).fold(self, |sum, (to, from)| {
            to.copy_from_slice(from);
            sum.feed(to)
        }))
    }

    pub fn sum(self) -> Sum {
        let padded = match self.pending {
            Some(high) => add64(self.sum, u64::from(u16::from_ne_bytes([high, 0]))),
            None => self.sum,
        };
        let [a, b, c, d, e, f, g, h] = padded.to_ne_bytes();
        let native = add16(
            add16(u16::from_ne_bytes([a, b]), u16::from_ne_bytes([c, d])),
            add16(u16::from_ne_bytes([e, f]), u16::from_ne_bytes([g, h])),
        );
        Sum(u16::from_be_bytes(native.to_ne_bytes()))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sum(u16);

impl Sum {
    pub fn of(bytes: &[u8]) -> Self {
        Accumulator::new().feed(bytes).sum()
    }

    pub const fn value(self) -> u16 {
        self.0
    }

    /// By summing, so the 0xFFFF RFC 1624 may write where 0x0000 is computed verifies.
    pub const fn verifies(self) -> bool {
        self.0 == 0xFFFF
    }

    pub const fn checksum(self) -> Checksum {
        Checksum(!self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Checksum(u16);

impl Checksum {
    pub fn of(bytes: &[u8]) -> Self {
        Sum::of(bytes).checksum()
    }

    pub const fn from_field(field: u16) -> Self {
        Self(field)
    }

    pub const fn value(self) -> u16 {
        self.0
    }

    pub const fn to_be_bytes(self) -> [u8; 2] {
        self.0.to_be_bytes()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PseudoHeader {
    pub source: Ipv4Addr,
    pub destination: Ipv4Addr,
    pub protocol: Protocol,
    pub length: u16,
}

impl PseudoHeader {
    pub fn accumulator(&self) -> Accumulator {
        let [s0, s1, s2, s3] = self.source.octets();
        let [d0, d1, d2, d3] = self.destination.octets();
        let [l0, l1] = self.length.to_be_bytes();
        Accumulator::new().feed(&[s0, s1, s2, s3, d0, d1, d2, d3, 0, self.protocol.number(), l0, l1])
    }
}
