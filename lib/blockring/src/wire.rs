//! The frames a session is opened with, before the rings carry anything.
//!
//! **One way in, one answer.** On the service's port a client opens a
//! partition by its unique GUID ([`MSG_OPEN`]), sending the session's region
//! with it, and is answered [`MSG_OPENED`] or [`MSG_REFUSED`] with a
//! [`Refusal`]. After `MSG_OPENED` the connection carries nothing but doorbell
//! bytes, each way.
//!
//! **What a connection may open is its [`Grant`]**, the badge the holder of
//! the service's acceptor minted its connector with, which the kernel stamps
//! on every connection through it: a listing names what the grant admits and
//! nothing else, an open of anything else is refused [`Refusal::NotGranted`],
//! and a connection with no grant reaches nothing.
//!
//! **The answer comes with the server's ends of the page.** A client looks at
//! the page the moment it hears, and one that opens the same region again
//! sends the cursors its last server left on it. So [`Opened::over`] makes a
//! server's ends and its answer together: nothing else makes the ends, and an
//! answer is otherwise only read off the wire ([`Opened::decode`]).

use toyos_transport::Word;

use crate::layout::{self, ServerRings, RING_WORDS};

/// Open the partition whose unique GUID is the payload, over the region sent
/// with it, if the connection's [`Grant`] admits it. Handles: the region.
pub const MSG_OPEN: u32 = 1;
/// The session is open; the payload is [`Opened`].
pub const MSG_OPENED: u32 = 2;
/// Refused; the payload is a [`Refusal`]'s word.
pub const MSG_REFUSED: u32 = 3;
/// What partitions the service serves that the connection's [`Grant`]
/// admits, for a client that finds its own by type or holds one partition's
/// grant: no payload, no handles; answered [`MSG_LISTED`].
pub const MSG_LIST: u32 = 4;
/// The answer to [`MSG_LIST`]: one [`Listed`] after another.
pub const MSG_LISTED: u32 = 5;

/// The bytes of a GUID payload.
pub const GUID_BYTES: usize = 16;

/// What an open was answered with: the partition's length in blocks, and its
/// unique GUID as the table stores it.
///
/// A server has one from [`Opened::over`], with its ends of the page:
///
/// ```
/// use core::sync::atomic::AtomicU32;
/// use toyos_blockring::{layout::RING_WORDS, wire::Opened};
/// let page: [AtomicU32; RING_WORDS] = core::array::from_fn(|_| AtomicU32::new(0));
/// let (_ends, _answer) = Opened::over(&page, 1, [0; 16]);
/// ```
///
/// and never without them:
///
/// ```compile_fail
/// let _answer = toyos_blockring::wire::Opened { blocks: 1, unique: [0; 16] };
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Opened {
    blocks: u64,
    unique: [u8; GUID_BYTES],
}

impl Opened {
    pub const BYTES: usize = 8 + GUID_BYTES;

    /// A server's ends of the session `page` it was sent, every word it owns
    /// set to 0, and the answer to send after: `blocks` of the partition
    /// `unique`.
    pub fn over<W: Word>(page: &[W; RING_WORDS], blocks: u64, unique: [u8; GUID_BYTES]) -> (ServerRings, Self) {
        (layout::server(page), Self { blocks, unique })
    }

    pub fn blocks(&self) -> u64 {
        self.blocks
    }

    pub fn unique(&self) -> [u8; GUID_BYTES] {
        self.unique
    }

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

/// One partition of the table a service drives, as [`MSG_LISTED`] carries
/// it: its unique and type GUIDs as the table stores them. Every entry the
/// grant admits is listed, one the service will not open among them, so a
/// client that finds its partition by type learns why from the open's refusal
/// rather than taking the partition for missing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Listed {
    pub unique: [u8; GUID_BYTES],
    pub kind: [u8; GUID_BYTES],
}

impl Listed {
    pub const BYTES: usize = 2 * GUID_BYTES;

    pub fn encode(&self) -> [u8; Self::BYTES] {
        let mut out = [0u8; Self::BYTES];
        out[..GUID_BYTES].copy_from_slice(&self.unique);
        out[GUID_BYTES..].copy_from_slice(&self.kind);
        out
    }

    /// Every entry of a listing, or `None` for one that is not whole entries.
    pub fn decode_all(bytes: &[u8]) -> Option<impl Iterator<Item = Self> + '_> {
        if !bytes.len().is_multiple_of(Self::BYTES) {
            return None;
        }
        let (chunks, _) = bytes.as_chunks::<{ Self::BYTES }>();
        Some(chunks.iter().map(|c| Self {
            unique: c[..GUID_BYTES].try_into().expect("sixteen bytes"),
            kind: c[GUID_BYTES..].try_into().expect("sixteen bytes"),
        }))
    }
}

/// What a connector to a block service reaches: the badge it was minted with
/// (`SYS_PORT_MINT`), whose bytes are [`Grant::encode`]'s.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grant {
    pub scope: Scope,
    /// A session it opens takes writes; one that does not answers every
    /// write `ReadOnly`, unissued.
    pub writes: bool,
}

/// Which partitions a [`Grant`] reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// The one partition whose unique GUID this is.
    Unique([u8; GUID_BYTES]),
    /// Every partition whose type GUID this is: a role found by type.
    Kind([u8; GUID_BYTES]),
}

impl Grant {
    pub const BYTES: usize = 2 + GUID_BYTES;

    pub fn encode(&self) -> [u8; Self::BYTES] {
        let (tag, guid) = match self.scope {
            Scope::Unique(guid) => (1, guid),
            Scope::Kind(guid) => (2, guid),
        };
        let mut out = [0u8; Self::BYTES];
        out[0] = tag;
        out[1] = u8::from(self.writes);
        out[2..].copy_from_slice(&guid);
        out
    }

    /// `None` for bytes no minter of this protocol stamps.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let bytes: &[u8; Self::BYTES] = bytes.try_into().ok()?;
        let guid: [u8; GUID_BYTES] = bytes[2..].try_into().ok()?;
        let scope = match bytes[0] {
            1 => Scope::Unique(guid),
            2 => Scope::Kind(guid),
            _ => return None,
        };
        let writes = match bytes[1] {
            0 => false,
            1 => true,
            _ => return None,
        };
        Some(Self { scope, writes })
    }

    /// Whether it reaches the partition whose unique GUID is `unique` and
    /// whose type GUID is `kind`.
    pub fn admits(&self, unique: [u8; GUID_BYTES], kind: [u8; GUID_BYTES]) -> bool {
        match self.scope {
            Scope::Unique(guid) => guid == unique,
            Scope::Kind(guid) => guid == kind,
        }
    }
}

/// Why an open was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// No partition this service drives carries that GUID; the zero GUID is
    /// every unused entry's and names none.
    NotFound,
    /// A session holds it already.
    Held,
    /// The partition is there and cannot be served: its range is not whole
    /// blocks, its GUID is on two entries, its table did not read, or its
    /// controller would not open — the answer to a listing then too.
    Unusable,
    /// The frame, its handles or its region are not what this protocol sends.
    Malformed,
    /// The service is holding as many sessions as it serves.
    Exhausted,
    /// The controller is on this machine and the kernel would not hand the
    /// service its claim: nothing on it is served, and a listing is refused
    /// the same.
    ClaimRefused,
    /// The connection's grant does not reach that partition, whether or not
    /// the service has it. A connection with no grant, or with bytes no
    /// minter of this protocol stamps, reaches none, and its listing is
    /// refused the same.
    NotGranted,
}

impl Refusal {
    pub const fn word(self) -> u32 {
        match self {
            Self::NotFound => 1,
            Self::Held => 2,
            Self::Unusable => 3,
            Self::Malformed => 4,
            Self::Exhausted => 5,
            Self::ClaimRefused => 6,
            Self::NotGranted => 7,
        }
    }

    pub const fn from_word(word: u32) -> Option<Self> {
        match word {
            1 => Some(Self::NotFound),
            2 => Some(Self::Held),
            3 => Some(Self::Unusable),
            4 => Some(Self::Malformed),
            5 => Some(Self::Exhausted),
            6 => Some(Self::ClaimRefused),
            7 => Some(Self::NotGranted),
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
        let listed = [
            Listed { unique: [1; GUID_BYTES], kind: [2; GUID_BYTES] },
            Listed { unique: [4; GUID_BYTES], kind: [5; GUID_BYTES] },
        ];
        let bytes: Vec<u8> = listed.iter().flat_map(|l| l.encode()).collect();
        assert_eq!(Listed::decode_all(&bytes).map(|all| all.collect::<Vec<_>>()), Some(listed.to_vec()));
        assert!(Listed::decode_all(&bytes[1..]).is_none());
        let opened = Opened { blocks: u64::MAX - 3, unique: [7; GUID_BYTES] };
        assert_eq!(Opened::decode(&opened.encode()), Some(opened));
        assert_eq!(Opened::decode(&opened.encode()[1..]), None);
        for r in [
            Refusal::NotFound,
            Refusal::Held,
            Refusal::Unusable,
            Refusal::Malformed,
            Refusal::Exhausted,
            Refusal::ClaimRefused,
            Refusal::NotGranted,
        ] {
            assert_eq!(Refusal::decode(&r.encode()), Some(r));
        }
        assert_eq!(Refusal::decode(&0u32.to_le_bytes()), None);
        assert_eq!(Refusal::decode(&[1, 0, 0]), None);
        assert_eq!(guid(&[0; 15]), None);
    }

    /// A unique grant reaches its one partition and a kind grant every
    /// partition of its type, never one that only shares the other GUID's
    /// bytes; and a badge decodes only as a minter of this protocol stamps it.
    #[test]
    fn a_grant_reaches_its_own_partitions_and_decodes_only_whole() {
        let (own, other, data) = ([1; GUID_BYTES], [2; GUID_BYTES], [9; GUID_BYTES]);
        let unique = Grant { scope: Scope::Unique(own), writes: false };
        assert!(unique.admits(own, data));
        assert!(!unique.admits(other, data));
        assert!(!unique.admits(data, own), "a unique grant read as a type");
        let kind = Grant { scope: Scope::Kind(data), writes: true };
        assert!(kind.admits(own, data) && kind.admits(other, data));
        assert!(!kind.admits(data, own), "a kind grant read as a unique GUID");

        for grant in [unique, kind] {
            assert_eq!(Grant::decode(&grant.encode()), Some(grant));
            assert_eq!(Grant::decode(&grant.encode()[1..]), None);
            let mut long = grant.encode().to_vec();
            long.push(0);
            assert_eq!(Grant::decode(&long), None);
        }
        let mut bad = kind.encode();
        bad[0] = 3;
        assert_eq!(Grant::decode(&bad), None);
        bad = kind.encode();
        bad[1] = 2;
        assert_eq!(Grant::decode(&bad), None);
    }
}
