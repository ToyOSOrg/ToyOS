//! The frames between the block service and a disk driver, on the `disk`
//! port the service serves and every disk driver receives.
//!
//! **A driver offers, the service routes.** A driver says who it is
//! ([`MSG_DRIVER`]), offers each disk it brought up with the partitions its
//! own table read names ([`MSG_OFFER`]), says once that every disk present at
//! its bring-up is offered ([`MSG_SETTLED`]), and that a disk left
//! ([`MSG_GONE`]). The service lists and admits what clients ask for against
//! those offers, and hands each opened session's client connection and region
//! to the driver offering its partition ([`MSG_SESSION`]): the driver answers
//! the client's open itself and serves the session, so a block never passes
//! through the service.
//!
//! **A disk is the driver's number for it**, unique for the life of its
//! connection; the service numbers disks across drivers itself.

use alloc::string::String;
use alloc::vec::Vec;

use crate::wire::GUID_BYTES;

/// The port the block service serves to disk drivers. Each driver's
/// connector is minted with the badge [`badge`] makes of who the supervisor
/// started it as, which is the driver's identity to the service.
pub const PORT: &str = "disk";

/// The badge a disk driver's connector is minted with: the supervisor's
/// number for the driver, which is its function's arrival.
pub fn badge(identity: u32) -> [u8; 4] {
    identity.to_le_bytes()
}

/// The identity a driver connection's badge names, `None` for bytes
/// [`badge`] does not make.
pub fn identity(badge: &[u8]) -> Option<u32> {
    Some(u32::from_le_bytes(badge.try_into().ok()?))
}

/// Driver to service, first: the driver's name for the service's lines,
/// UTF-8, at most [`MAX_LABEL`] bytes.
pub const MSG_DRIVER: u32 = 1;
/// Driver to service: one disk and every partition its table names, an
/// [`Offer`].
pub const MSG_OFFER: u32 = 2;
/// Driver to service, once: every disk present at its bring-up is offered,
/// or its controller is one it cannot use. A [`Settled`] word.
pub const MSG_SETTLED: u32 = 3;
/// Driver to service: the disk numbered by the payload's four bytes left, and
/// every session on it ended.
pub const MSG_GONE: u32 = 4;
/// Service to driver: serve this session, a [`Session`]. Handles, in order:
/// the client's connection, and the region it sent with its open.
pub const MSG_SESSION: u32 = 5;
/// Service to driver: describe this disk to the client, a [`disk`]-number
/// payload. Handles: the client's connection, which the driver answers on.
pub const MSG_DESCRIBE: u32 = 6;
/// Service to driver: carry out this designation for the client, a
/// [`Designation`]. Handles: the client's connection, which the driver
/// answers on.
pub const MSG_DESIGNATE: u32 = 7;

pub const MAX_LABEL: usize = 64;
/// The partitions one offer may name: a GPT's whole entry array at the
/// standard size.
pub const MAX_PARTS: usize = 128;

/// One partition of an offered disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Part {
    pub unique: [u8; GUID_BYTES],
    pub kind: [u8; GUID_BYTES],
    /// Its length in blocks, or `None` for one the driver will not serve: a
    /// range that is not whole blocks, or one its own table refuses.
    pub blocks: Option<u64>,
}

impl Part {
    pub const BYTES: usize = 2 * GUID_BYTES + 1 + 8;

    fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.unique);
        out.extend_from_slice(&self.kind);
        out.push(u8::from(self.blocks.is_some()));
        out.extend_from_slice(&self.blocks.unwrap_or(0).to_le_bytes());
    }

    fn decode(bytes: &[u8; Self::BYTES]) -> Option<Self> {
        let blocks = u64::from_le_bytes(bytes[33..].try_into().ok()?);
        let blocks = match bytes[32] {
            0 if blocks == 0 => None,
            1 => Some(blocks),
            _ => return None,
        };
        Some(Self {
            unique: bytes[..GUID_BYTES].try_into().ok()?,
            kind: bytes[GUID_BYTES..2 * GUID_BYTES].try_into().ok()?,
            blocks,
        })
    }
}

/// One disk, as [`MSG_OFFER`] carries it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offer {
    pub disk: u32,
    pub parts: Vec<Part>,
}

impl Offer {
    pub fn encode(&self) -> Vec<u8> {
        assert!(self.parts.len() <= MAX_PARTS, "an offer of {} partitions", self.parts.len());
        let mut out = Vec::with_capacity(4 + self.parts.len() * Part::BYTES);
        out.extend_from_slice(&self.disk.to_le_bytes());
        self.parts.iter().for_each(|p| p.encode(&mut out));
        out
    }

    /// `None` for bytes that are not a disk number and whole entries, or
    /// more entries than [`MAX_PARTS`].
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let (disk, rest) = bytes.split_first_chunk::<4>()?;
        let (entries, torn) = rest.as_chunks::<{ Part::BYTES }>();
        if !torn.is_empty() || entries.len() > MAX_PARTS {
            return None;
        }
        let parts = entries.iter().map(Part::decode).collect::<Option<Vec<_>>>()?;
        Some(Self { disk: u32::from_le_bytes(*disk), parts })
    }
}

/// How a driver's bring-up ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Settled {
    /// Every disk present at its bring-up is offered.
    Offered,
    /// Its controller cannot be used: what it would have offered is not
    /// absent, and a listing says so.
    Unusable,
}

impl Settled {
    pub fn encode(self) -> [u8; 4] {
        match self {
            Self::Offered => 0u32,
            Self::Unusable => 1,
        }
        .to_le_bytes()
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        match u32::from_le_bytes(bytes.try_into().ok()?) {
            0 => Some(Self::Offered),
            1 => Some(Self::Unusable),
            _ => None,
        }
    }
}

/// A session the service admitted, as [`MSG_SESSION`] carries it: on the
/// driver's disk `disk`, the partition `unique`, taking writes or not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Session {
    pub disk: u32,
    pub unique: [u8; GUID_BYTES],
    pub writes: bool,
}

impl Session {
    pub const BYTES: usize = 4 + GUID_BYTES + 1;

    pub fn encode(&self) -> [u8; Self::BYTES] {
        let mut out = [0u8; Self::BYTES];
        out[..4].copy_from_slice(&self.disk.to_le_bytes());
        out[4..4 + GUID_BYTES].copy_from_slice(&self.unique);
        out[4 + GUID_BYTES] = u8::from(self.writes);
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let bytes: &[u8; Self::BYTES] = bytes.try_into().ok()?;
        let writes = match bytes[4 + GUID_BYTES] {
            0 => false,
            1 => true,
            _ => return None,
        };
        Some(Self {
            disk: u32::from_le_bytes(bytes[..4].try_into().ok()?),
            unique: bytes[4..4 + GUID_BYTES].try_into().ok()?,
            writes,
        })
    }
}

/// A designation the service admitted, as [`MSG_DESIGNATE`] carries it: on
/// the driver's disk `disk`, the partition `unique`; `active` for a partition
/// the running system is on, which the driver refuses; `commit` to write
/// rather than judge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Designation {
    pub disk: u32,
    pub unique: [u8; GUID_BYTES],
    pub active: bool,
    pub commit: bool,
}

impl Designation {
    pub const BYTES: usize = 4 + GUID_BYTES + 2;

    pub fn encode(&self) -> [u8; Self::BYTES] {
        let mut out = [0u8; Self::BYTES];
        out[..4].copy_from_slice(&self.disk.to_le_bytes());
        out[4..20].copy_from_slice(&self.unique);
        out[20] = u8::from(self.active);
        out[21] = u8::from(self.commit);
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let b: &[u8; Self::BYTES] = bytes.try_into().ok()?;
        let flag = |v: u8| match v {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        };
        Some(Self {
            disk: u32::from_le_bytes(b[..4].try_into().ok()?),
            unique: b[4..20].try_into().ok()?,
            active: flag(b[20])?,
            commit: flag(b[21])?,
        })
    }
}

/// A [`MSG_DRIVER`] payload: the driver's name for the service's lines, which
/// never says who the driver is.
pub fn label(bytes: &[u8]) -> Option<String> {
    (1..=MAX_LABEL).contains(&bytes.len()).then(|| core::str::from_utf8(bytes).ok().map(String::from))?
}

/// A [`MSG_GONE`] payload: the disk's number.
pub fn disk(bytes: &[u8]) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_frame_survives_its_bytes_and_nothing_else_decodes() {
        let offer = Offer {
            disk: 7,
            parts: vec![
                Part { unique: [1; GUID_BYTES], kind: [2; GUID_BYTES], blocks: Some(u64::MAX) },
                Part { unique: [3; GUID_BYTES], kind: [4; GUID_BYTES], blocks: None },
            ],
        };
        let bytes = offer.encode();
        assert_eq!(Offer::decode(&bytes), Some(offer.clone()));
        assert_eq!(Offer::decode(&bytes[..bytes.len() - 1]), None, "a torn entry");
        assert_eq!(Offer::decode(&bytes[..3]), None, "no disk number");
        assert_eq!(Offer::decode(&7u32.to_le_bytes()), Some(Offer { disk: 7, parts: vec![] }), "a disk with no table");
        let mut flag = bytes.clone();
        flag[4 + 32] = 2;
        assert_eq!(Offer::decode(&flag), None, "a usable flag that is neither");
        let mut unusable_with_length = bytes;
        let second = 4 + Part::BYTES;
        unusable_with_length[second + 33] = 1;
        assert_eq!(Offer::decode(&unusable_with_length), None, "an unserved partition with a length");
        let many = Offer { disk: 0, parts: vec![offer.parts[0]; MAX_PARTS] };
        assert!(Offer::decode(&many.encode()).is_some());
        let mut past = many.encode();
        past.extend_from_within(4..4 + Part::BYTES);
        assert_eq!(Offer::decode(&past), None, "past a GPT's whole array");

        for settled in [Settled::Offered, Settled::Unusable] {
            assert_eq!(Settled::decode(&settled.encode()), Some(settled));
        }
        assert_eq!(Settled::decode(&2u32.to_le_bytes()), None);

        let session = Session { disk: 3, unique: [9; GUID_BYTES], writes: true };
        assert_eq!(Session::decode(&session.encode()), Some(session));
        let mut bad = session.encode();
        bad[Session::BYTES - 1] = 2;
        assert_eq!(Session::decode(&bad), None);
        assert_eq!(Session::decode(&session.encode()[1..]), None);

        assert_eq!(label(b"nvme 01:00.0").as_deref(), Some("nvme 01:00.0"));
        assert_eq!(label(b""), None);
        assert_eq!(label(&[b'a'; MAX_LABEL + 1]), None);
        assert_eq!(label(&[0xFF]), None);
        assert_eq!(disk(&5u32.to_le_bytes()), Some(5));
        assert_eq!(disk(&[0; 3]), None);
        assert_eq!(identity(&badge(0x0102_0304)), Some(0x0102_0304));
        assert_eq!(identity(&[1, 2, 3]), None);
        assert_eq!(identity(&[1, 2, 3, 4, 5]), None);
    }
}
