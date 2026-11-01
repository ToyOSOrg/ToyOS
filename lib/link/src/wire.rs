//! The frames a link is opened with, before the region carries anything.
//!
//! **One way in, one answer.** A driver says who it is ([`MSG_HELLO`]) and
//! sends the region it made with it; the stack answers [`MSG_OPENED`] with
//! the link's two bells, both of which it made, or [`MSG_REFUSED`] with a
//! [`Refusal`]. After `MSG_OPENED` the connection has nothing left to carry,
//! and either side's waker going ends the link.

use alloc::string::String;
use alloc::vec::Vec;

/// A device's own address: an individual one, its first byte's low bit,
/// IEEE 802's individual/group bit, clear.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Address([u8; 6]);

impl Address {
    /// `None` for a group's address, the broadcast one among them.
    pub fn new(bytes: [u8; 6]) -> Option<Self> {
        (bytes[0] & 1 == 0).then_some(Self(bytes))
    }

    pub fn bytes(self) -> [u8; 6] {
        self.0
    }
}

/// Driver to stack: a [`Hello`]. Handles: the region.
pub const MSG_HELLO: u32 = 1;
/// Stack to driver: the link is open. No payload. Handles
/// ([`OPENED_HANDLES`]): the waker the driver rings the stack with, then the
/// bell the stack rings the driver on.
pub const MSG_OPENED: u32 = 2;
/// Stack to driver: the opened answer's handles.
pub const OPENED_HANDLES: usize = 2;
/// Stack to driver: refused, a [`Refusal`]'s word.
pub const MSG_REFUSED: u32 = 3;

/// The longest driver name a hello carries.
pub const MAX_LABEL: usize = 32;

/// What a link is carried over, which the stack ranks links by: wired before
/// wireless.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Medium {
    Wired,
    Wireless,
}

impl Medium {
    fn encode(self) -> u8 {
        match self {
            Self::Wired => 1,
            Self::Wireless => 2,
        }
    }

    fn decode(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::Wired),
            2 => Some(Self::Wireless),
            _ => None,
        }
    }
}

/// Who a driver is: its device's address, which is an individual one, what
/// the link is carried over, and its name for the stack's lines: ASCII
/// letters, digits and `-`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hello {
    pub mac: Address,
    pub medium: Medium,
    pub label: String,
}

impl Hello {
    pub const MAX_BYTES: usize = 7 + MAX_LABEL;

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(7 + self.label.len());
        out.extend_from_slice(&self.mac.bytes());
        out.push(self.medium.encode());
        out.extend_from_slice(self.label.as_bytes());
        out
    }

    /// `None` for bytes [`Self::encode`] does not make: too short, a group's
    /// address, no medium, or a name that is empty, too long or not one.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let (mac, rest) = bytes.split_first_chunk::<6>()?;
        let mac = Address::new(*mac)?;
        let (&medium, label) = rest.split_first()?;
        let medium = Medium::decode(medium)?;
        let named = |b: &u8| b.is_ascii_alphanumeric() || *b == b'-';
        if label.is_empty() || label.len() > MAX_LABEL || !label.iter().all(named) {
            return None;
        }
        Some(Self { mac, medium, label: String::from(core::str::from_utf8(label).ok()?) })
    }
}

/// Why the stack would not open a link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// A link of this address is open and alive.
    Open,
    /// The stack drives the most links it drives.
    Full,
    /// The stack ended this address's last link for what its driver wrote.
    /// The driver ends on it, so its supervisor's restart budget bounds a
    /// driver that keeps writing what no driver writes.
    Faulted,
    /// A hello this protocol does not make, or a region that is not one.
    Malformed,
}

impl Refusal {
    pub fn encode(self) -> [u8; 4] {
        match self {
            Self::Open => 1u32,
            Self::Malformed => 2,
            Self::Faulted => 3,
            Self::Full => 4,
        }
        .to_le_bytes()
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        match u32::from_le_bytes(bytes.try_into().ok()?) {
            1 => Some(Self::Open),
            2 => Some(Self::Malformed),
            3 => Some(Self::Faulted),
            4 => Some(Self::Full),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mac(bytes: [u8; 6]) -> Address {
        Address::new(bytes).unwrap()
    }

    #[test]
    fn a_hello_reads_back_as_written() {
        let hello = Hello { mac: mac([0x52, 0x54, 0, 0x12, 0x34, 0x56]), medium: Medium::Wired, label: "virtio-net".into() };
        assert_eq!(Hello::decode(&hello.encode()), Some(hello));
        let longest = Hello { mac: mac([2; 6]), medium: Medium::Wireless, label: "a".repeat(MAX_LABEL) };
        assert_eq!(longest.encode().len(), Hello::MAX_BYTES);
        assert_eq!(Hello::decode(&longest.encode()), Some(longest));
    }

    /// Each way a hello's bytes are not one a driver makes is refused, a
    /// group's address among them: the node is made with the address.
    #[test]
    fn a_hello_no_driver_writes_is_refused() {
        let named = |mac: [u8; 6], label: &[u8]| [mac.as_slice(), &[1], label].concat();
        assert_eq!(Hello::decode(&[2, 0, 0, 0, 0]), None, "short of an address");
        assert_eq!(Hello::decode(&[2, 0, 0, 0, 0, 0]), None, "no medium");
        for medium in [0, 3, 0xff] {
            assert_eq!(Hello::decode(&[[2, 0, 0, 0, 0, 0, medium].as_slice(), b"i219"].concat()), None, "medium {medium}");
        }
        assert_eq!(Hello::decode(&named([0x01, 0x00, 0x5e, 0, 0, 0xfb], b"i219")), None, "a group's address");
        assert_eq!(Hello::decode(&named([0xff; 6], b"i219")), None, "the broadcast address");
        assert_eq!(Hello::decode(&named([2; 6], &[b'a'; MAX_LABEL + 1])), None, "a name too long");
        assert_eq!(Hello::decode(&named([2; 6], b"")), None, "no name");
        for name in [b"i219\n".as_slice(), b"two words", b"\xff", b"e1000e."] {
            assert_eq!(Hello::decode(&named([2; 6], name)), None, "the name {name:?}");
        }
    }

    #[test]
    fn every_refusal_reads_back_and_no_other_word_does() {
        for why in [Refusal::Open, Refusal::Full, Refusal::Malformed, Refusal::Faulted] {
            assert_eq!(Refusal::decode(&why.encode()), Some(why));
        }
        assert_eq!(Refusal::decode(&0u32.to_le_bytes()), None);
        assert_eq!(Refusal::decode(&[1, 0, 0]), None);
    }
}
