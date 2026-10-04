//! Sequence numbers and timestamps live modulo 2^32 (RFC 9293 §3.4, RFC 7323 §5.2), so neither has
//! `Ord`: every comparison is an offset from a reference, and two values exactly 2^31 apart are
//! neither before nor after each other. The class removed is one value classified two ways by two
//! code paths.

use toyos_net_wire::tcp::SeqNum;

const HALF: u32 = 1 << 31;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Seq(u32);

impl Seq {
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u32 {
        self.0
    }

    pub const fn add(self, n: u32) -> Self {
        Self(self.0.wrapping_add(n))
    }

    pub const fn sub(self, n: u32) -> Self {
        Self(self.0.wrapping_sub(n))
    }

    /// `(self − from) mod 2^32`.
    pub const fn since(self, from: Self) -> u32 {
        self.0.wrapping_sub(from.0)
    }

    pub const fn before(self, other: Self) -> bool {
        let d = other.since(self);
        d != 0 && d < HALF
    }

    pub const fn after(self, other: Self) -> bool {
        other.before(self)
    }

    pub const fn at_or_before(self, other: Self) -> bool {
        self.0 == other.0 || self.before(other)
    }

    pub const fn at_or_after(self, other: Self) -> bool {
        other.at_or_before(self)
    }

    /// `(self − from) mod 2^32` lies in `[0, len)`.
    pub const fn within(self, from: Self, len: u32) -> bool {
        self.since(from) < len
    }

    /// The later of two values the stack knows lie within 2^30 of each other.
    pub const fn later(self, other: Self) -> Self {
        if other.after(self) {
            other
        } else {
            self
        }
    }

    /// The earlier of two values the stack knows lie within 2^30 of each other.
    pub const fn earlier(self, other: Self) -> Self {
        if other.before(self) {
            other
        } else {
            self
        }
    }
}

impl From<SeqNum> for Seq {
    fn from(value: SeqNum) -> Self {
        Self(value.get())
    }
}

impl From<Seq> for SeqNum {
    fn from(value: Seq) -> Self {
        SeqNum::new(value.0)
    }
}

/// A TSval or TSecr: ordered like a sequence number (RFC 7323 §5.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp(pub u32);

impl Stamp {
    pub const fn before(self, other: Self) -> bool {
        Seq(self.0).before(Seq(other.0))
    }

    pub const fn at_or_after(self, other: Self) -> bool {
        Seq(self.0).at_or_after(Seq(other.0))
    }

    pub const fn since(self, from: Self) -> u32 {
        self.0.wrapping_sub(from.0)
    }
}
