//! The frames a session is opened with, before the rings carry anything.
//!
//! **One way in, one answer.** On the service's port a client opens a
//! partition by its unique GUID ([`MSG_OPEN`]), sending the session's region
//! with it, and is answered [`MSG_OPENED`] or [`MSG_REFUSED`] with a
//! [`Refusal`]. `MSG_OPENED` carries the session's doorbells, and the
//! connection carries nothing after it.
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

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use toyos::transport::Word;

use crate::layout::{self, ServerRings, RING_WORDS};

/// Open the partition whose unique GUID is the payload, over the region sent
/// with it, if the connection's [`Grant`] admits it. Handles: the region.
pub const MSG_OPEN: u32 = 1;
/// The session is open; the payload is [`Opened`], and the handles are
/// [`OPENED_HANDLES`].
pub const MSG_OPENED: u32 = 2;
/// What [`MSG_OPENED`] carries, in order: the waker the client rings for its
/// requests, and the bell the server rings for its answers.
pub const OPENED_HANDLES: usize = 2;
/// Refused; the payload is a [`Refusal`]'s word.
pub const MSG_REFUSED: u32 = 3;
/// What partitions the service serves that the connection's [`Grant`]
/// admits, for a client that finds its own by type or holds one partition's
/// grant: no payload, no handles; answered [`MSG_LISTED`].
pub const MSG_LIST: u32 = 4;
/// The answer to [`MSG_LIST`]: one [`Listed`] after another.
pub const MSG_LISTED: u32 = 5;
/// What one disk's table holds, for a [`Badge::Tables`] connection: the
/// payload is the service's number for the disk, four bytes. Its driver
/// answers [`MSG_DISK`], a [`MSG_SLOT`] for each used entry and
/// [`MSG_DESCRIBED`]; or [`MSG_REFUSED`].
pub const MSG_DESCRIBE: u32 = 6;
/// A [`DiskHead`].
pub const MSG_DISK: u32 = 7;
/// A [`SlotLine`].
pub const MSG_SLOT: u32 = 8;
/// The description is whole. No payload.
pub const MSG_DESCRIBED: u32 = 9;
/// Designate the machine's DATA partition, for a [`Badge::DesignateData`]
/// connection: payload one byte, 1 to write the stamp and 0 to judge only.
/// Answered [`MSG_DESIGNATED`] or [`MSG_REFUSED`].
pub const MSG_DESIGNATE: u32 = 10;
/// A [`Designated`].
pub const MSG_DESIGNATED: u32 = 11;

/// The bytes of a GUID payload.
pub const GUID_BYTES: usize = 16;

/// How long the service waits for its disk drivers to settle, from its start
/// or a settled driver's end, before it answers a listing or an open with
/// what it knows. Policy: a stick's bring-up is a port's reset and
/// enumeration and a Bulk-Only disk's first commands, each bounded far below
/// this.
pub const DRIVER_BOUND_NS: u64 = 30_000_000_000;

/// How long a client waits for a listing's or an open's answer before it
/// takes the service for wedged: the service's wait for its drivers, and as
/// long again for the driver's answer to an open it hands over.
pub const ANSWER_BOUND_NS: u64 = 2 * DRIVER_BOUND_NS;

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

/// One partition a service serves, as [`MSG_LISTED`] carries it: the
/// service's number for the disk it is on, and its unique and type GUIDs as
/// the disk's table stores them. Every entry the grant admits is listed, one
/// the service will not open among them, so a client that finds its
/// partition by type learns why from the open's refusal rather than taking
/// the partition for missing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Listed {
    pub disk: u32,
    pub unique: [u8; GUID_BYTES],
    pub kind: [u8; GUID_BYTES],
}

impl Listed {
    pub const BYTES: usize = 4 + 2 * GUID_BYTES;

    pub fn encode(&self) -> [u8; Self::BYTES] {
        let mut out = [0u8; Self::BYTES];
        out[..4].copy_from_slice(&self.disk.to_le_bytes());
        out[4..4 + GUID_BYTES].copy_from_slice(&self.unique);
        out[4 + GUID_BYTES..].copy_from_slice(&self.kind);
        out
    }

    /// Every entry of a listing, or `None` for one that is not whole entries.
    pub fn decode_all(bytes: &[u8]) -> Option<impl Iterator<Item = Self> + '_> {
        if !bytes.len().is_multiple_of(Self::BYTES) {
            return None;
        }
        let (chunks, _) = bytes.as_chunks::<{ Self::BYTES }>();
        Some(chunks.iter().map(|c| Self {
            disk: u32::from_le_bytes(c[..4].try_into().expect("four bytes")),
            unique: c[4..4 + GUID_BYTES].try_into().expect("sixteen bytes"),
            kind: c[4 + GUID_BYTES..].try_into().expect("sixteen bytes"),
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
    /// blocks, its GUID is on two disks, or its controller would not open. A
    /// listing that finds nothing it admits is refused the same while a disk
    /// driver cannot use its controller or never settled.
    Unusable,
    /// The frame, its handles or its region are not what this protocol sends.
    Malformed,
    /// The service is holding as many sessions as it serves.
    Exhausted,
    /// A disk controller is on this machine and no driver of it started — the
    /// kernel would not hand over its claim, or the driver would not spawn:
    /// nothing on it is served, and a listing that finds nothing it admits is
    /// refused the same.
    ClaimRefused,
    /// The connection's grant does not reach that partition, whether or not
    /// the service has it. A connection with no grant, or with bytes no
    /// minter of this protocol stamps, reaches none, and its listing is
    /// refused the same.
    NotGranted,
    /// The partition backs the running system, which no authorization
    /// changes beneath it.
    Active,
    /// The device did not do what the operation needed of it.
    Device,
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
            Self::Active => 8,
            Self::Device => 9,
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
            8 => Some(Self::Active),
            9 => Some(Self::Device),
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

/// What a connector to the block service reaches: the badge the supervisor
/// minted it with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Badge {
    /// Sessions on the partitions a [`Grant`] admits.
    Partitions(Grant),
    /// Every disk's table, read: a listing of every partition and each disk's
    /// description. No session and no change.
    Tables,
    /// Designations of the machine's DATA partition, resolved when one is
    /// asked for: the one partition of `TOYOS_DATA`'s type the service
    /// serves. Nothing else.
    DesignateData,
}

impl Badge {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::Partitions(grant) => grant.encode().to_vec(),
            Self::Tables => vec![3],
            Self::DesignateData => vec![4],
        }
    }

    /// `None` for bytes no minter of this protocol stamps.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        match bytes.first()? {
            1 | 2 => Grant::decode(bytes).map(Self::Partitions),
            3 => (bytes.len() == 1).then_some(Self::Tables),
            4 => (bytes.len() == 1).then_some(Self::DesignateData),
            _ => None,
        }
    }

    /// Whether a listing through it names the partition `unique` of type `kind`.
    pub fn lists(&self, unique: [u8; GUID_BYTES], kind: [u8; GUID_BYTES]) -> bool {
        match self {
            Self::Partitions(grant) => grant.admits(unique, kind),
            Self::Tables => true,
            Self::DesignateData => false,
        }
    }
}

/// The longest copy state a [`DiskHead`] carries.
pub const MAX_STATE: usize = 200;

/// One disk as its driver describes it: its size, and what each copy of its
/// table is — `valid` or why not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiskHead {
    pub lba_bytes: u32,
    pub sectors: u64,
    /// Both copies valid and the same table.
    pub agree: bool,
    pub primary: String,
    pub backup: String,
}

impl DiskHead {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(18 + self.primary.len() + self.backup.len() + 4);
        out.extend_from_slice(&self.lba_bytes.to_le_bytes());
        out.extend_from_slice(&self.sectors.to_le_bytes());
        out.push(u8::from(self.agree));
        for state in [&self.primary, &self.backup] {
            let state = &state.as_bytes()[..floor_char(state, MAX_STATE)];
            out.extend_from_slice(&(state.len() as u16).to_le_bytes());
            out.extend_from_slice(state);
        }
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let (lba_bytes, rest) = bytes.split_first_chunk::<4>()?;
        let (sectors, rest) = rest.split_first_chunk::<8>()?;
        let (&[agree], rest) = rest.split_first_chunk::<1>()?;
        let agree = match agree {
            0 => false,
            1 => true,
            _ => return None,
        };
        let (primary, rest) = text(rest)?;
        let (backup, rest) = text(rest)?;
        rest.is_empty().then(|| Self {
            lba_bytes: u32::from_le_bytes(*lba_bytes),
            sectors: u64::from_le_bytes(*sectors),
            agree,
            primary,
            backup,
        })
    }
}

/// The longest prefix of `s` within `max` bytes that ends on a character.
fn floor_char(s: &str, max: usize) -> usize {
    (0..=s.len().min(max)).rev().find(|&i| s.is_char_boundary(i)).unwrap_or(0)
}

/// A length-prefixed UTF-8 string of at most [`MAX_STATE`] bytes, and what follows it.
fn text(bytes: &[u8]) -> Option<(String, &[u8])> {
    let (len, rest) = bytes.split_first_chunk::<2>()?;
    let len = u16::from_le_bytes(*len) as usize;
    if len > MAX_STATE {
        return None;
    }
    let (s, rest) = rest.split_at_checked(len)?;
    Some((core::str::from_utf8(s).ok()?.into(), rest))
}

/// One used entry of the table in force, as [`MSG_SLOT`] carries it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotLine {
    pub index: u32,
    pub kind: [u8; GUID_BYTES],
    pub unique: [u8; GUID_BYTES],
    pub first: u64,
    pub last: u64,
    pub attributes: u64,
    pub name: [u16; 36],
    /// A session or the driver's own work holds it now.
    pub held: bool,
    /// The driver serves sessions on it.
    pub served: bool,
}

impl SlotLine {
    pub const BYTES: usize = 4 + 2 * GUID_BYTES + 3 * 8 + 72 + 2;

    pub fn encode(&self) -> [u8; Self::BYTES] {
        let mut out = [0u8; Self::BYTES];
        out[..4].copy_from_slice(&self.index.to_le_bytes());
        out[4..20].copy_from_slice(&self.kind);
        out[20..36].copy_from_slice(&self.unique);
        out[36..44].copy_from_slice(&self.first.to_le_bytes());
        out[44..52].copy_from_slice(&self.last.to_le_bytes());
        out[52..60].copy_from_slice(&self.attributes.to_le_bytes());
        for (pair, unit) in out[60..132].as_chunks_mut::<2>().0.iter_mut().zip(self.name) {
            *pair = unit.to_le_bytes();
        }
        out[132] = u8::from(self.held);
        out[133] = u8::from(self.served);
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let b: &[u8; Self::BYTES] = bytes.try_into().ok()?;
        let flag = |v: u8| match v {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        };
        let mut name = [0u16; 36];
        for (unit, pair) in name.iter_mut().zip(b[60..132].as_chunks::<2>().0) {
            *unit = u16::from_le_bytes(*pair);
        }
        Some(Self {
            index: u32::from_le_bytes(b[..4].try_into().ok()?),
            kind: b[4..20].try_into().ok()?,
            unique: b[20..36].try_into().ok()?,
            first: u64::from_le_bytes(b[36..44].try_into().ok()?),
            last: u64::from_le_bytes(b[44..52].try_into().ok()?),
            attributes: u64::from_le_bytes(b[52..60].try_into().ok()?),
            name,
            held: flag(b[132])?,
            served: flag(b[133])?,
        })
    }
}

/// What a designation came to, once it was admitted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Designated {
    /// The stamp is on the medium and read back.
    Written,
    /// A judge-only request: the stamp would be written.
    WouldWrite,
    /// A stamp naming this partition's size is there already; nothing written.
    Already,
    /// The partition holds a volume of ToyOS's own; nothing written.
    Volume,
    /// The partition carries another format's signature, named; nothing written.
    Foreign(String),
}

impl Designated {
    pub fn encode(&self) -> Vec<u8> {
        let (word, text): (u32, &str) = match self {
            Self::Written => (1, ""),
            Self::WouldWrite => (2, ""),
            Self::Already => (3, ""),
            Self::Volume => (4, ""),
            Self::Foreign(name) => (5, &name[..floor_char(name, MAX_STATE)]),
        };
        [&word.to_le_bytes()[..], text.as_bytes()].concat()
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let (word, text) = bytes.split_first_chunk::<4>()?;
        let empty = text.is_empty();
        match u32::from_le_bytes(*word) {
            1 if empty => Some(Self::Written),
            2 if empty => Some(Self::WouldWrite),
            3 if empty => Some(Self::Already),
            4 if empty => Some(Self::Volume),
            5 if text.len() <= MAX_STATE => Some(Self::Foreign(core::str::from_utf8(text).ok()?.into())),
            _ => None,
        }
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
            Listed { disk: 0, unique: [1; GUID_BYTES], kind: [2; GUID_BYTES] },
            Listed { disk: u32::MAX, unique: [4; GUID_BYTES], kind: [5; GUID_BYTES] },
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
