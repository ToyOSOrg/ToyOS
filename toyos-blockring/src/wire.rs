//! The frames a session is opened and a grant is bound with, before the rings
//! carry anything.
//!
//! **Three ways in, one answer each.** On the service's own port a client
//! opens any partition by its unique GUID ([`MSG_OPEN`]), sending the
//! session's region with it; a holder of that port can also bind a fresh
//! port's acceptor to one partition ([`MSG_BIND`]), and a connection on the
//! bound port attaches to that partition and no other ([`MSG_ATTACH`]). Every
//! one is answered [`MSG_OPENED`]/[`MSG_BOUND`] or [`MSG_REFUSED`] with a
//! [`Refusal`], and after `MSG_OPENED` the connection carries nothing but
//! doorbell bytes, each way.

/// Open the partition whose unique GUID is the payload, over the region sent
/// with it. Handles: the region.
pub const MSG_OPEN: u32 = 1;
/// Serve the partition whose unique GUID is the payload on the acceptor sent
/// with it, for as long as this service runs. Handles: the acceptor.
pub const MSG_BIND: u32 = 2;
/// On a bound port: open its partition over the region sent with it. No
/// payload. Handles: the region.
pub const MSG_ATTACH: u32 = 3;
/// The session is open; the payload is [`Opened`].
pub const MSG_OPENED: u32 = 4;
/// The port is bound; the payload is [`Opened`], for the partition it serves.
pub const MSG_BOUND: u32 = 5;
/// Refused; the payload is a [`Refusal`]'s word.
pub const MSG_REFUSED: u32 = 6;

/// The bytes of a GUID payload.
pub const GUID_BYTES: usize = 16;

/// What an open or a bind was answered with: the partition's length in
/// blocks, and its unique GUID as the table stores it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Opened {
    pub blocks: u64,
    pub unique: [u8; GUID_BYTES],
}

impl Opened {
    pub const BYTES: usize = 8 + GUID_BYTES;

    pub fn encode(&self) -> [u8; Self::BYTES] {
        let mut out = [0u8; Self::BYTES];
        out[..8].copy_from_slice(&self.blocks.to_le_bytes());
        out[8..].copy_from_slice(&self.unique);
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != Self::BYTES {
            return None;
        }
        let blocks = u64::from_le_bytes(bytes[..8].try_into().ok()?);
        let unique = bytes[8..].try_into().ok()?;
        Some(Self { blocks, unique })
    }
}

/// Why an open, a bind or an attach was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// No partition this service drives carries that GUID; the zero GUID is
    /// every unused entry's and names none.
    NotFound,
    /// A session or a binding holds it already.
    Held,
    /// The partition is there and cannot be served: its range is not whole
    /// blocks, its GUID is on two entries, or its table did not read.
    Unusable,
    /// The frame, its handles or its region are not what this protocol sends.
    Malformed,
    /// The service is holding as many sessions as it serves.
    Exhausted,
}

impl Refusal {
    pub const fn word(self) -> u32 {
        match self {
            Self::NotFound => 1,
            Self::Held => 2,
            Self::Unusable => 3,
            Self::Malformed => 4,
            Self::Exhausted => 5,
        }
    }

    pub const fn from_word(word: u32) -> Option<Self> {
        match word {
            1 => Some(Self::NotFound),
            2 => Some(Self::Held),
            3 => Some(Self::Unusable),
            4 => Some(Self::Malformed),
            5 => Some(Self::Exhausted),
            _ => None,
        }
    }

    pub fn encode(self) -> [u8; 4] {
        self.word().to_le_bytes()
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        Self::from_word(u32::from_le_bytes(bytes.try_into().ok()?))
    }
}

/// A GUID payload, exactly [`GUID_BYTES`] long.
pub fn guid(bytes: &[u8]) -> Option<[u8; GUID_BYTES]> {
    bytes.try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opened_and_refusal_survive_their_bytes_and_nothing_else_decodes() {
        let opened = Opened { blocks: u64::MAX - 3, unique: [7; GUID_BYTES] };
        assert_eq!(Opened::decode(&opened.encode()), Some(opened));
        assert_eq!(Opened::decode(&opened.encode()[1..]), None);
        for r in [Refusal::NotFound, Refusal::Held, Refusal::Unusable, Refusal::Malformed, Refusal::Exhausted] {
            assert_eq!(Refusal::decode(&r.encode()), Some(r));
        }
        assert_eq!(Refusal::decode(&0u32.to_le_bytes()), None);
        assert_eq!(Refusal::decode(&[1, 0, 0]), None);
        assert_eq!(guid(&[0; 15]), None);
    }
}
