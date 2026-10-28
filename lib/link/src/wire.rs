//! The frames a link is opened with, before the region carries anything.
//!
//! **One way in, one answer.** A driver says who it is ([`MSG_HELLO`]) and
//! sends the region it made with it; the stack answers [`MSG_OPENED`] or
//! [`MSG_REFUSED`] with a [`Refusal`]. After `MSG_OPENED` the connection
//! carries nothing but doorbell bytes, each way, and its hang-up ends the
//! link.

use alloc::string::String;
use alloc::vec::Vec;

/// Driver to stack: a [`Hello`]. Handles: the region.
pub const MSG_HELLO: u32 = 1;
/// Stack to driver: the link is open. No payload.
pub const MSG_OPENED: u32 = 2;
/// Stack to driver: refused, a [`Refusal`]'s word.
pub const MSG_REFUSED: u32 = 3;

/// The longest driver name a hello carries.
pub const MAX_LABEL: usize = 32;

/// Who a driver is: its device's address, and its name for the stack's lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hello {
    pub mac: [u8; 6],
    pub label: String,
}

impl Hello {
    pub const MAX_BYTES: usize = 6 + MAX_LABEL;

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(6 + self.label.len());
        out.extend_from_slice(&self.mac);
        out.extend_from_slice(self.label.as_bytes());
        out
    }

    /// `None` for bytes [`Self::encode`] does not make: too short, a name
    /// too long, or one that is not UTF-8.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let (mac, label) = bytes.split_first_chunk::<6>()?;
        if label.len() > MAX_LABEL {
            return None;
        }
        Some(Self { mac: *mac, label: String::from(core::str::from_utf8(label).ok()?) })
    }
}

/// Why the stack would not open a link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The stack drives one link, and another holds it, or held it with
    /// another address (`issues/netstack-drives-one-card.md`).
    OneLink,
    /// A hello this protocol does not make, or a region that is not one.
    Malformed,
}

impl Refusal {
    pub fn encode(self) -> [u8; 4] {
        match self {
            Self::OneLink => 1u32,
            Self::Malformed => 2,
        }
        .to_le_bytes()
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        match u32::from_le_bytes(bytes.try_into().ok()?) {
            1 => Some(Self::OneLink),
            2 => Some(Self::Malformed),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hello_reads_back_as_written() {
        let hello = Hello { mac: [0x52, 0x54, 0, 0x12, 0x34, 0x56], label: "virtio-net".into() };
        assert_eq!(Hello::decode(&hello.encode()), Some(hello));
        let unnamed = Hello { mac: [2; 6], label: String::new() };
        assert_eq!(Hello::decode(&unnamed.encode()), Some(unnamed));
    }

    #[test]
    fn a_hello_no_driver_writes_is_refused() {
        assert_eq!(Hello::decode(&[1, 2, 3, 4, 5]), None, "short of an address");
        assert_eq!(Hello::decode(&[[0u8; 6].as_slice(), &[b'a'; MAX_LABEL + 1]].concat()), None, "a name too long");
        assert_eq!(Hello::decode(&[0, 0, 0, 0, 0, 0, 0xff]), None, "a name that is not UTF-8");
    }

    #[test]
    fn every_refusal_reads_back_and_no_other_word_does() {
        for why in [Refusal::OneLink, Refusal::Malformed] {
            assert_eq!(Refusal::decode(&why.encode()), Some(why));
        }
        assert_eq!(Refusal::decode(&0u32.to_le_bytes()), None);
        assert_eq!(Refusal::decode(&[1, 0, 0]), None);
    }
}
