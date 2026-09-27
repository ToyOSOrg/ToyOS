//! The Internet checksum (RFC 1071), its incremental update (RFC 1624) and the IPv4 pseudo-header; a received checksum verifies when its covered sum is 0xFFFF.

use core::net::Ipv4Addr;

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
        let mut sum = self.sum;
        let rest = match (self.pending, bytes.split_first()) {
            (Some(high), Some((&low, rest))) => {
                sum = add64(sum, u64::from(u16::from_be_bytes([high, low])));
                rest
            }
            (Some(_), None) => return self,
            (None, _) => bytes,
        };
        let (words, tail) = rest.as_chunks::<8>();
        for word in words {
            sum = add64(sum, u64::from_be_bytes(*word));
        }
        let (pairs, odd) = tail.as_chunks::<2>();
        for pair in pairs {
            sum = add64(sum, u64::from(u16::from_be_bytes(*pair)));
        }
        Self { sum, pending: odd.first().copied() }
    }

    pub fn sum(self) -> Sum {
        let padded = match self.pending {
            Some(high) => add64(self.sum, u64::from(u16::from_be_bytes([high, 0]))),
            None => self.sum,
        };
        let [a, b, c, d, e, f, g, h] = padded.to_be_bytes();
        Sum(add16(
            add16(u16::from_be_bytes([a, b]), u16::from_be_bytes([c, d])),
            add16(u16::from_be_bytes([e, f]), u16::from_be_bytes([g, h])),
        ))
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

    /// RFC 1624 equation 3: equal to recomputation whenever the covered data holds a nonzero byte.
    #[must_use]
    pub fn replace(self, old: [u8; 2], new: [u8; 2]) -> Self {
        let m = u16::from_be_bytes(old);
        let m_new = u16::from_be_bytes(new);
        Self(!add16(add16(!self.0, !m), m_new))
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
