//! The frames a link is opened with, before the region carries anything.
//!
//! **One way in, one answer.** A driver says who it is ([`MSG_HELLO`]) and
//! sends the region it made with it; the stack answers [`MSG_OPENED`] or
//! [`MSG_REFUSED`] with a [`Refusal`]. After `MSG_OPENED` the connection
//! carries nothing but doorbell bytes, each way, and its hang-up ends the
//! link.

use alloc::string::String;
use alloc::vec::Vec;

use toyos_net_wire::ethernet::{IndividualMac, MacAddr};

/// Driver to stack: a [`Hello`]. Handles: the region.
pub const MSG_HELLO: u32 = 1;
/// Stack to driver: the link is open. No payload.
pub const MSG_OPENED: u32 = 2;
/// Stack to driver: refused, a [`Refusal`]'s word.
pub const MSG_REFUSED: u32 = 3;

/// The longest driver name a hello carries.
pub const MAX_LABEL: usize = 32;

/// Who a driver is: its device's address, which is an individual one, and its
/// name for the stack's lines: ASCII letters, digits and `-`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hello {
    pub mac: IndividualMac,
    pub label: String,
}

impl Hello {
    pub const MAX_BYTES: usize = 6 + MAX_LABEL;

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(6 + self.label.len());
        out.extend_from_slice(&self.mac.get().0);
        out.extend_from_slice(self.label.as_bytes());
        out
    }

    /// `None` for bytes [`Self::encode`] does not make: too short, a group's
    /// address, or a name that is empty, too long or not one.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let (mac, label) = bytes.split_first_chunk::<6>()?;
        let mac = IndividualMac::new(MacAddr(*mac))?;
        let named = |b: &u8| b.is_ascii_alphanumeric() || *b == b'-';
        if label.is_empty() || label.len() > MAX_LABEL || !label.iter().all(named) {
            return None;
        }
        Some(Self { mac, label: String::from(core::str::from_utf8(label).ok()?) })
    }
}

/// Why the stack would not open a link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The stack drives one link, and its address is another's, or a link of
    /// this address is open and alive (`issues/netstack-drives-one-card.md`).
    OneLink,
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
            Self::OneLink => 1u32,
            Self::Malformed => 2,
            Self::Faulted => 3,
        }
        .to_le_bytes()
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        match u32::from_le_bytes(bytes.try_into().ok()?) {
            1 => Some(Self::OneLink),
            2 => Some(Self::Malformed),
            3 => Some(Self::Faulted),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mac(bytes: [u8; 6]) -> IndividualMac {
        IndividualMac::new(MacAddr(bytes)).unwrap()
    }

    #[test]
    fn a_hello_reads_back_as_written() {
        let hello = Hello { mac: mac([0x52, 0x54, 0, 0x12, 0x34, 0x56]), label: "virtio-net".into() };
        assert_eq!(Hello::decode(&hello.encode()), Some(hello));
        let longest = Hello { mac: mac([2; 6]), label: "a".repeat(MAX_LABEL) };
        assert_eq!(Hello::decode(&longest.encode()), Some(longest));
    }

    /// Each way a hello's bytes are not one a driver makes is refused, a
    /// group's address among them: the node is made with the address.
    #[test]
    fn a_hello_no_driver_writes_is_refused() {
        let named = |mac: [u8; 6], label: &[u8]| [mac.as_slice(), label].concat();
        assert_eq!(Hello::decode(&[2, 0, 0, 0, 0]), None, "short of an address");
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
        for why in [Refusal::OneLink, Refusal::Malformed, Refusal::Faulted] {
            assert_eq!(Refusal::decode(&why.encode()), Some(why));
        }
        assert_eq!(Refusal::decode(&0u32.to_le_bytes()), None);
        assert_eq!(Refusal::decode(&[1, 0, 0]), None);
    }
}
