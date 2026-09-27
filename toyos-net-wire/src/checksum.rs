//! The Internet checksum (RFC 1071), its incremental update (RFC 1624 §3), and
//! the IPv4 pseudo-header that UDP and TCP sum first (RFC 768; RFC 9293 §3.1).
//!
//! The sum is taken over big-endian 16-bit words, an odd final byte being the
//! high byte of a word whose low byte is zero. It does not depend on how the
//! input is split: [`Accumulator`] carries a piece's odd last byte into the
//! next piece, and only [`Accumulator::sum`] pads. Carries are added back at
//! every step and the final fold is complete, so no accumulator width or input
//! length loses one.
//!
//! A received checksum is valid when the sum over everything it covers, the
//! checksum field included, is 0xFFFF ([`Sum::verifies`]). Recomputing and
//! comparing would refuse the 0xFFFF a correct RFC 1624 update can write where
//! 0x0000 is computed. An update replaces one aligned 16-bit word, so a byte at
//! an odd offset is updated as the word that contains it and never as a word
//! of its own.

use core::net::Ipv4Addr;

use crate::ipv4::Protocol;

/// Ones'-complement addition of two 16-bit words.
fn add16(a: u16, b: u16) -> u16 {
    let (sum, carry) = a.overflowing_add(b);
    // A carry leaves `sum` at most 0xFFFE, so adding it back cannot carry.
    sum.wrapping_add(u16::from(carry))
}

/// Ones'-complement addition at 64 bits: 2^64 ≡ 1 modulo 0xFFFF, so an
/// end-around carry here is the same carry the 16-bit sum would take.
fn add64(a: u64, b: u64) -> u64 {
    let (sum, carry) = a.overflowing_add(b);
    sum.wrapping_add(u64::from(carry))
}

/// A running ones'-complement sum over any number of byte pieces.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Accumulator {
    sum: u64,
    /// The odd last byte of the pieces so far: the high byte of the next word.
    pending: Option<u8>,
}

impl Accumulator {
    pub const fn new() -> Self {
        Self { sum: 0, pending: None }
    }

    /// The sum with `bytes` appended to what came before.
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

    /// The folded sum, an odd final byte padded with zero.
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

/// A folded 16-bit ones'-complement sum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sum(u16);

impl Sum {
    /// The sum of `bytes`.
    pub fn of(bytes: &[u8]) -> Self {
        Accumulator::new().feed(bytes).sum()
    }

    pub const fn value(self) -> u16 {
        self.0
    }

    /// Whether a sum over covered bytes, checksum field included, says they
    /// arrived intact.
    pub const fn verifies(self) -> bool {
        self.0 == 0xFFFF
    }

    /// The checksum a header carries for bytes that sum to this.
    pub const fn checksum(self) -> Checksum {
        Checksum(!self.0)
    }
}

/// The value of a checksum field: the ones' complement of a sum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Checksum(u16);

impl Checksum {
    /// The checksum of `bytes`.
    pub fn of(bytes: &[u8]) -> Self {
        Sum::of(bytes).checksum()
    }

    /// A checksum as read from a header.
    pub const fn from_field(field: u16) -> Self {
        Self(field)
    }

    pub const fn value(self) -> u16 {
        self.0
    }

    pub const fn to_be_bytes(self) -> [u8; 2] {
        self.0.to_be_bytes()
    }

    /// The checksum after one covered, word-aligned 16-bit field changes from
    /// `old` to `new`: RFC 1624 equation 3, `~(~HC + ~m + m')`. It equals a
    /// computation from scratch, 0x0000 included, whenever the covered data
    /// holds a nonzero byte (RFC 1624 §3's premise, true of every IPv4 header
    /// and every pseudo-header); over zeros alone it may give 0x0000 where
    /// recomputation gives 0xFFFF.
    #[must_use]
    pub fn replace(self, old: [u8; 2], new: [u8; 2]) -> Self {
        let m = u16::from_be_bytes(old);
        let m_new = u16::from_be_bytes(new);
        Self(!add16(add16(!self.0, !m), m_new))
    }

    /// [`Self::replace`] for an address, as its two words.
    #[must_use]
    pub fn replace_address(self, old: Ipv4Addr, new: Ipv4Addr) -> Self {
        let [a, b, c, d] = old.octets();
        let [e, f, g, h] = new.octets();
        self.replace([a, b], [e, f]).replace([c, d], [g, h])
    }
}

/// The IPv4 pseudo-header a UDP or TCP checksum covers first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PseudoHeader {
    pub source: Ipv4Addr,
    pub destination: Ipv4Addr,
    pub protocol: Protocol,
    /// UDP's length field, or TCP's header plus data.
    pub length: u16,
}

impl PseudoHeader {
    /// An accumulator that has summed the pseudo-header.
    pub fn accumulator(&self) -> Accumulator {
        let [s0, s1, s2, s3] = self.source.octets();
        let [d0, d1, d2, d3] = self.destination.octets();
        let [l0, l1] = self.length.to_be_bytes();
        Accumulator::new().feed(&[s0, s1, s2, s3, d0, d1, d2, d3, 0, self.protocol.number(), l0, l1])
    }
}
