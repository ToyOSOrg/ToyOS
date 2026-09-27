//! The slot table: which partitions make each slot, which slot is marked, and
//! what the running system asks of the loader's next pass.
//!
//! It lives on its own partition of type `toyos_gpt::Guid::TOYOS_SLOTS`, in two copies at
//! blocks 0 and 1, and **a writer writes the copy that is not the current
//! one**, with a sequence one past it. A write that tears leaves that copy
//! unreadable and the current one standing, so moving the mark is atomic on a
//! device that tears a block: the machine boots the old mark or the new one,
//! never neither.
//!
//! ```text
//! magic "TOYOSLOT" | format u32 | marked u32 | sequence u64
//! then per slot: present u32 | 0 u32 | boot guid [16] | root guid [16] | version u64
//! then the request: next u32 (0 none, 1 a slot, 2 an ESP) | slot u32 | esp guid [16]
//!                   | first u32 (0 or 1)
//! then crc32 u32 over everything before it                   (TABLE_BYTES)
//! ```
//!
//! The version a slot records is what its writer installed, and is the
//! updater's to compare against; the loader trusts nothing here but which
//! partitions to read, which slot is marked and what the request asks, and
//! judges each slot by its own signed header.
//!
//! **The request is how the running system reaches the firmware's boot
//! variables**, which nothing after `ExitBootServices` here writes: the kernel
//! never maps the runtime services, so the loader, which runs with boot
//! services, writes them for it ([`Request`]). A field that is not the one
//! value a writer leaves there is refused rather than read, so a table either
//! asks exactly one thing or is no table.

/// The unit the table's copies are written in.
pub const BLOCK: usize = 4096;

/// Blocks the table's partition must hold: its two copies.
pub const COPIES: u64 = 2;

const MAGIC: [u8; 8] = *b"TOYOSLOT";
const FORMAT: u32 = 2;
const SLOT_BYTES: usize = 4 + 4 + 16 + 16 + 8;
const REQUEST_AT: usize = 8 + 4 + 4 + 8 + 2 * SLOT_BYTES;
const REQUEST_BYTES: usize = 4 + 4 + 16 + 4;
const BODY_BYTES: usize = REQUEST_AT + REQUEST_BYTES;
/// A copy's bytes, checksum included; the rest of its block is zero.
pub const TABLE_BYTES: usize = BODY_BYTES + 4;

/// Where each slot's kernel, boot parameter and signed header are on its FAT
/// partition, as paths from the volume's root.
pub const KERNEL_FILE: &str = "toyos/kernel.elf";
pub const CMDLINE_FILE: &str = "toyos/cmdline";
pub const SIGNED_FILE: &str = "toyos/image.sig";

/// A slot's two partitions, by their unique GUIDs as a GPT entry stores them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    pub boot: [u8; 16],
    pub root: [u8; 16],
    /// The version its writer installed; `0` for a slot nothing has been installed in.
    pub version: u64,
}

/// Slot `A` or slot `B`, by index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Which {
    A,
    B,
}

impl Which {
    pub const fn index(self) -> usize {
        match self {
            Self::A => 0,
            Self::B => 1,
        }
    }

    pub const fn other(self) -> Self {
        match self {
            Self::A => Self::B,
            Self::B => Self::A,
        }
    }

    pub const fn letter(self) -> char {
        match self {
            Self::A => 'A',
            Self::B => 'B',
        }
    }

    pub const fn from_letter(c: char) -> Option<Self> {
        match c {
            'A' => Some(Self::A),
            'B' => Some(Self::B),
            _ => None,
        }
    }
}

/// What to boot once, at the next pass that boots anything, and never again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    /// One of this disk's slots, whether or not it is the marked one: the
    /// bench's trial of an image the machine does not keep.
    Slot(Which),
    /// The EFI system partition with this unique GUID, on any disk the
    /// firmware sees, by its removable-media path: the firmware's `BootNext`,
    /// which is how the owner reaches another stick without a keyboard.
    Esp([u8; 16]),
}

/// What the running system asks of the loader's next pass.
///
/// **Each field is acted on once**: the pass that acts on it writes the table
/// again without it before it acts, so a pass that dies after the write has
/// lost the request rather than repeating it, and a pass that cannot write
/// the table acts on nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Request {
    pub next: Option<Next>,
    /// Put this loader's own entry first in the firmware's `BootOrder`,
    /// making it where the firmware has none: how a machine is taken over.
    pub first: bool,
}

impl Request {
    pub const NONE: Self = Self { next: None, first: false };

    pub const fn is_empty(&self) -> bool {
        self.next.is_none() && !self.first
    }
}

/// One copy of the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Table {
    pub sequence: u64,
    pub marked: Which,
    /// Slot `A` then slot `B`; `None` for a machine built with one slot.
    pub slots: [Option<Slot>; 2],
    pub request: Request,
}

/// Why a copy is not a table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unreadable {
    Magic,
    Format(u32),
    Checksum,
    /// It marks a slot it does not carry, or a mark that is no slot.
    Mark(u32),
    /// Its request is none a writer makes: an unknown kind, a slot it does
    /// not carry, or a field that is not the one value its kind leaves.
    Request(&'static str),
}

impl core::fmt::Display for Unreadable {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match *self {
            Self::Magic => write!(f, "no slot table"),
            Self::Format(n) => write!(f, "a slot table of format {n}, and this reads {FORMAT}"),
            Self::Checksum => write!(f, "a slot table whose checksum does not hold, which is a torn write"),
            Self::Mark(n) => write!(f, "a slot table marking slot {n}, which it does not carry"),
            Self::Request(why) => write!(f, "a slot table whose request {why}"),
        }
    }
}

const NEXT_NONE: u32 = 0;
const NEXT_SLOT: u32 = 1;
const NEXT_ESP: u32 = 2;

impl Table {
    pub fn slot(&self, which: Which) -> Option<Slot> {
        self.slots[which.index()]
    }

    pub fn encode(&self) -> [u8; BLOCK] {
        let mut out = [0u8; BLOCK];
        out[..8].copy_from_slice(&MAGIC);
        out[8..12].copy_from_slice(&FORMAT.to_le_bytes());
        out[12..16].copy_from_slice(&(self.marked.index() as u32).to_le_bytes());
        out[16..24].copy_from_slice(&self.sequence.to_le_bytes());
        for (i, slot) in self.slots.iter().enumerate() {
            let at = 24 + i * SLOT_BYTES;
            if let Some(slot) = slot {
                out[at..at + 4].copy_from_slice(&1u32.to_le_bytes());
                out[at + 8..at + 24].copy_from_slice(&slot.boot);
                out[at + 24..at + 40].copy_from_slice(&slot.root);
                out[at + 40..at + 48].copy_from_slice(&slot.version.to_le_bytes());
            }
        }
        let at = REQUEST_AT;
        match self.request.next {
            None => {}
            Some(Next::Slot(which)) => {
                out[at..at + 4].copy_from_slice(&NEXT_SLOT.to_le_bytes());
                out[at + 4..at + 8].copy_from_slice(&(which.index() as u32).to_le_bytes());
            }
            Some(Next::Esp(guid)) => {
                out[at..at + 4].copy_from_slice(&NEXT_ESP.to_le_bytes());
                out[at + 8..at + 24].copy_from_slice(&guid);
            }
        }
        out[at + 24..at + 28].copy_from_slice(&u32::from(self.request.first).to_le_bytes());
        let crc = crc32(&out[..BODY_BYTES]);
        out[BODY_BYTES..TABLE_BYTES].copy_from_slice(&crc.to_le_bytes());
        out
    }

    pub fn decode(block: &[u8; BLOCK]) -> Result<Self, Unreadable> {
        if block[..8] != MAGIC {
            return Err(Unreadable::Magic);
        }
        let word = |at: usize| u32::from_le_bytes(block[at..at + 4].try_into().expect("four bytes"));
        let format = word(8);
        if format != FORMAT {
            return Err(Unreadable::Format(format));
        }
        if crc32(&block[..BODY_BYTES]) != word(BODY_BYTES) {
            return Err(Unreadable::Checksum);
        }
        let mut slots = [None; 2];
        for (i, slot) in slots.iter_mut().enumerate() {
            let at = 24 + i * SLOT_BYTES;
            if word(at) == 1 {
                *slot = Some(Slot {
                    boot: block[at + 8..at + 24].try_into().expect("sixteen bytes"),
                    root: block[at + 24..at + 40].try_into().expect("sixteen bytes"),
                    version: u64::from_le_bytes(block[at + 40..at + 48].try_into().expect("eight bytes")),
                });
            }
        }
        let marked = match word(12) {
            0 if slots[0].is_some() => Which::A,
            1 if slots[1].is_some() => Which::B,
            other => return Err(Unreadable::Mark(other)),
        };
        let sequence = u64::from_le_bytes(block[16..24].try_into().expect("eight bytes"));
        let request = Self::request(block, &slots)?;
        Ok(Self { sequence, marked, slots, request })
    }

    /// The request a copy carries, each field held to the one value its kind
    /// leaves there.
    fn request(block: &[u8; BLOCK], slots: &[Option<Slot>; 2]) -> Result<Request, Unreadable> {
        let at = REQUEST_AT;
        let word = |at: usize| u32::from_le_bytes(block[at..at + 4].try_into().expect("four bytes"));
        let guid: [u8; 16] = block[at + 8..at + 24].try_into().expect("sixteen bytes");
        let (kind, slot) = (word(at), word(at + 4));
        let next = match kind {
            NEXT_NONE if slot == 0 && guid == [0; 16] => None,
            NEXT_NONE => return Err(Unreadable::Request("asks nothing next and names something to boot")),
            NEXT_SLOT if guid != [0; 16] => return Err(Unreadable::Request("names a slot and an ESP at once")),
            NEXT_SLOT => match slot {
                0 if slots[0].is_some() => Some(Next::Slot(Which::A)),
                1 if slots[1].is_some() => Some(Next::Slot(Which::B)),
                _ => return Err(Unreadable::Request("names a slot the table does not carry")),
            },
            NEXT_ESP if slot != 0 => return Err(Unreadable::Request("names an ESP and a slot at once")),
            NEXT_ESP if guid == [0; 16] => return Err(Unreadable::Request("names an ESP by no GUID")),
            NEXT_ESP => Some(Next::Esp(guid)),
            _ => return Err(Unreadable::Request("asks for a kind of boot there is none of")),
        };
        let first = match word(at + 24) {
            0 => false,
            1 => true,
            _ => return Err(Unreadable::Request("asks for the boot order with a word that is neither 0 nor 1")),
        };
        Ok(Request { next, first })
    }
}

/// The table two copies make: the readable one with the higher sequence, and
/// which copy that is. Both unreadable is the first copy's reason.
pub fn current(copies: [&[u8; BLOCK]; 2]) -> Result<(Table, usize), Unreadable> {
    match (Table::decode(copies[0]), Table::decode(copies[1])) {
        (Ok(a), Ok(b)) if b.sequence > a.sequence => Ok((b, 1)),
        (Ok(a), _) => Ok((a, 0)),
        (Err(_), Ok(b)) => Ok((b, 1)),
        (Err(why), Err(_)) => Err(why),
    }
}

/// What a writer puts where, to make `next` the table: the copy that is not
/// `current`'s, one sequence past it.
pub fn next_write(current: (Table, usize), mut next: Table) -> (usize, [u8; BLOCK]) {
    next.sequence = current.0.sequence + 1;
    (1 - current.1, next.encode())
}

/// The labels `/system/bin/init` endows a `slots` grant under, and
/// `/system/bin/update` takes it by: the slot table's partition, and the idle
/// slot's FAT volume and ROOT, each a partition claim.
pub const TABLE_LABEL: &str = "slots:table";
pub const BOOT_LABEL: &str = "slots:boot";
pub const ROOT_LABEL: &str = "slots:root";

/// The one program a build lets hold the grant: `/system/bin/update`.
pub const HOLDER: &str = "update";

/// Why a machine has no slot to write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoIdle {
    /// The table carries one slot, and that is the one running.
    OneSlot,
    /// Neither slot's ROOT is the one the kernel holds: this table is not
    /// the one this boot came from.
    NotThisBoot,
    /// The idle slot names a partition that is no idle slot's.
    Stray { part: &'static str, why: Stray },
}

/// Why a partition the idle slot names is not one a grant may claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stray {
    /// It is one of the running slot's.
    Running,
    /// The machine lists no partition with its GUID.
    Unlisted,
    /// It is on another disk than the one this boot runs from.
    OtherDisk,
    /// Its type is not the one a slot's partition of that kind carries.
    Type,
    /// Two listed partitions carry its GUID.
    Duplicate,
}

impl core::fmt::Display for NoIdle {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::OneSlot => write!(f, "the slot table carries one slot, and the machine runs it"),
            Self::NotThisBoot => write!(f, "neither slot's ROOT is the one this boot runs"),
            Self::Stray { part, why } => {
                let why = match why {
                    Stray::Running => "is one of the running slot's",
                    Stray::Unlisted => "is no partition this machine lists",
                    Stray::OtherDisk => "is on another disk than the one this boot runs from",
                    Stray::Type => "is not of the type a slot's partition of that kind carries",
                    Stray::Duplicate => "names a GUID two listed partitions carry",
                };
                write!(f, "the idle slot's {part} {why}")
            }
        }
    }
}

/// A partition as the machine's inventory lists it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Listed {
    pub device: u32,
    pub type_guid: [u8; 16],
    pub unique_guid: [u8; 16],
}

/// The partition types a slot's volume and its ROOT carry, as a GPT entry
/// stores them (`toyos_gpt::Guid::TOYOS_BOOT` and `TOYOS_ROOT`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Kinds {
    pub boot: [u8; 16],
    pub root: [u8; 16],
}

/// The idle slot a grant may claim, given the ROOT the kernel holds
/// (`running`) and every partition the machine lists.
///
/// **The table is the grantee's to write**, so nothing it names is taken on
/// its word: each of the idle slot's two partitions must be on the running
/// ROOT's disk, of its kind's type, and neither of the running slot's — else
/// the holder of one grant could name its next one anywhere.
pub fn grant(table: &Table, running: &Listed, listed: &[Listed], kinds: Kinds) -> Result<(Which, Slot), NoIdle> {
    let (idle, slot) = idle(table, &running.unique_guid)?;
    let runs = table.slot(idle.other()).expect("`idle` found the running slot in the table");
    for (part, guid, kind) in [("volume", slot.boot, kinds.boot), ("ROOT", slot.root, kinds.root)] {
        let mut named = listed.iter().filter(|p| p.unique_guid == guid);
        let why = match (named.next(), named.next()) {
            _ if guid == runs.boot || guid == runs.root => Some(Stray::Running),
            (None, _) => Some(Stray::Unlisted),
            (Some(p), None) if p.device != running.device => Some(Stray::OtherDisk),
            (Some(p), None) if p.type_guid != kind => Some(Stray::Type),
            (Some(_), None) => None,
            // The kernel refuses a claim of a GUID two tables carry; so does this.
            (Some(_), Some(_)) => Some(Stray::Duplicate),
        };
        if let Some(why) = why {
            return Err(NoIdle::Stray { part, why });
        }
    }
    Ok((idle, slot))
}

/// The slot the machine is not running, given the ROOT the kernel holds —
/// **what makes the running slot unwritable by construction**: the grant is
/// only ever the other one.
pub fn idle(table: &Table, running_root: &[u8; 16]) -> Result<(Which, Slot), NoIdle> {
    let running = [Which::A, Which::B]
        .into_iter()
        .find(|&w| table.slot(w).is_some_and(|s| s.root == *running_root))
        .ok_or(NoIdle::NotThisBoot)?;
    let idle = running.other();
    table.slot(idle).map(|slot| (idle, slot)).ok_or(NoIdle::OneSlot)
}

/// CRC-32 (IEEE 802.3, reflected), the checksum GPT uses; a torn write is what
/// it catches, not an adversary.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(marked: Which, sequence: u64) -> Table {
        let slot = |n: u8| Some(Slot { boot: [n; 16], root: [n + 1; 16], version: u64::from(n) });
        Table { sequence, marked, slots: [slot(1), slot(3)], request: Request::NONE }
    }

    /// `block` with its checksum made to hold again, so a test bends one field
    /// and the refusal is that field's rather than the checksum's.
    fn resealed(mut block: [u8; BLOCK]) -> [u8; BLOCK] {
        let crc = crc32(&block[..BODY_BYTES]);
        block[BODY_BYTES..TABLE_BYTES].copy_from_slice(&crc.to_le_bytes());
        block
    }

    /// **A request reads back as it was asked, and a request no writer makes
    /// is no table**: an unknown kind, a slot the table does not carry, a slot
    /// and an ESP at once, an ESP of no GUID, and a boot-order word that is
    /// neither 0 nor 1.
    #[test]
    fn a_request_reads_back_and_one_no_writer_makes_is_refused() {
        let t = table(Which::A, 2);
        for request in [
            Request::NONE,
            Request { next: Some(Next::Slot(Which::B)), first: false },
            Request { next: Some(Next::Slot(Which::A)), first: true },
            Request { next: Some(Next::Esp([0x5A; 16])), first: false },
            Request { next: None, first: true },
        ] {
            let asked = Table { request, ..t };
            assert_eq!(Table::decode(&asked.encode()), Ok(asked), "{request:?}");
        }
        let at = REQUEST_AT;
        let bent = |bend: &dyn Fn(&mut [u8; BLOCK])| {
            let mut block = Table { request: Request { next: Some(Next::Slot(Which::B)), first: false }, ..t }.encode();
            bend(&mut block);
            Table::decode(&resealed(block))
        };
        let refused = |why| Err(Unreadable::Request(why));
        assert_eq!(bent(&|b| b[at] = 3), refused("asks for a kind of boot there is none of"));
        assert_eq!(bent(&|b| b[at + 4] = 2), refused("names a slot the table does not carry"));
        assert_eq!(bent(&|b| b[at + 8] = 1), refused("names a slot and an ESP at once"));
        assert_eq!(bent(&|b| b[at + 24] = 2), refused("asks for the boot order with a word that is neither 0 nor 1"));
        assert_eq!(bent(&|b| b[at] = 0), refused("asks nothing next and names something to boot"));
        assert_eq!(bent(&|b| { b[at] = 2; b[at + 4] = 0 }), refused("names an ESP by no GUID"));
        let one = Table { slots: [t.slots[0], None], request: Request { next: Some(Next::Slot(Which::A)), first: false }, ..t };
        let mut names_absent = one.encode();
        names_absent[at + 4] = 1;
        assert_eq!(Table::decode(&resealed(names_absent)), refused("names a slot the table does not carry"));
    }

    /// The check value every CRC-32 is held to.
    #[test]
    fn the_checksum_is_crc32() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn a_table_reads_back_and_a_bent_one_is_refused() {
        let t = table(Which::B, 9);
        assert_eq!(Table::decode(&t.encode()), Ok(t));
        let mut torn = t.encode();
        torn[40] ^= 1;
        assert_eq!(Table::decode(&torn), Err(Unreadable::Checksum));
        assert_eq!(Table::decode(&[0; BLOCK]), Err(Unreadable::Magic));
        let one = Table { slots: [t.slots[0], None], ..t };
        let mut marks_absent = one.encode();
        // Re-encode with a mark on the absent slot, checksum and all.
        marks_absent[12..16].copy_from_slice(&1u32.to_le_bytes());
        let crc = crc32(&marks_absent[..BODY_BYTES]);
        marks_absent[BODY_BYTES..TABLE_BYTES].copy_from_slice(&crc.to_le_bytes());
        assert_eq!(Table::decode(&marks_absent), Err(Unreadable::Mark(1)));
    }

    /// **The write that tears is never the one read**: the writer writes the
    /// copy that is not current, so a torn one leaves the old table, and a
    /// whole one is the new table by its sequence.
    #[test]
    fn moving_the_mark_is_atomic_against_a_torn_copy() {
        let old = table(Which::A, 4);
        let (a, b) = (old.encode(), table(Which::A, 3).encode());
        let now = current([&a, &b]).expect("a table");
        assert_eq!(now, (old, 0));
        let (copy, written) = next_write(now, table(Which::B, 0));
        assert_eq!(copy, 1);
        assert_eq!(current([&a, &written]).expect("a table").0.marked, Which::B);
        assert_eq!(current([&a, &written]).expect("a table").0.sequence, 5);
        let mut torn = written;
        torn[100] ^= 0xFF;
        assert_eq!(current([&a, &torn]).expect("a table"), (old, 0));
        assert_eq!(current([&[0; BLOCK], &[0; BLOCK]]), Err(Unreadable::Magic));
    }

    /// The grant is the slot the kernel is not running from, and a table this
    /// boot did not come from, or one with nothing idle, grants nothing.
    #[test]
    fn the_idle_slot_is_the_one_whose_root_the_kernel_does_not_hold() {
        let t = table(Which::A, 1);
        assert_eq!(idle(&t, &[2; 16]).map(|(w, _)| w), Ok(Which::B));
        assert_eq!(idle(&t, &[4; 16]).map(|(w, _)| w), Ok(Which::A));
        assert_eq!(idle(&t, &[9; 16]), Err(NoIdle::NotThisBoot));
        let one = Table { slots: [t.slots[0], None], ..t };
        assert_eq!(idle(&one, &[2; 16]), Err(NoIdle::OneSlot));
    }

    /// **A table the grantee wrote names nothing outside an idle slot**: a
    /// partition of the running slot, another disk's, another type's — the
    /// ESP's or the log partition's — or one the machine does not list is
    /// refused by name, and the table the build wrote is granted.
    #[test]
    fn a_grant_claims_only_an_idle_slot_on_the_running_disk() {
        const KINDS: Kinds = Kinds { boot: [0xB0; 16], root: [0xA0; 16] };
        const ESP: [u8; 16] = [0xE5; 16];
        let at = |device, type_guid, unique_guid| Listed { device, type_guid, unique_guid };
        let running = at(0, KINDS.root, [2; 16]);
        let listed = [
            running,
            at(0, KINDS.boot, [1; 16]),
            at(0, KINDS.boot, [3; 16]),
            at(0, KINDS.root, [4; 16]),
            at(0, [0xEF; 16], ESP),
            at(1, KINDS.boot, [5; 16]),
        ];
        let t = table(Which::A, 1);
        assert_eq!(grant(&t, &running, &listed, KINDS).map(|(w, _)| w), Ok(Which::B));
        let bent = |boot: [u8; 16], root: [u8; 16]| {
            let mut t = t;
            t.slots[1] = Some(Slot { boot, root, version: 0 });
            grant(&t, &running, &listed, KINDS)
        };
        let stray = |part, why| Err(NoIdle::Stray { part, why });
        assert_eq!(bent([1; 16], [4; 16]), stray("volume", Stray::Running), "the running slot's volume");
        assert_eq!(bent([3; 16], [2; 16]), stray("ROOT", Stray::Running), "the running ROOT");
        assert_eq!(bent(ESP, [4; 16]), stray("volume", Stray::Type), "the ESP");
        assert_eq!(bent([3; 16], [3; 16]), stray("ROOT", Stray::Type), "a volume named as ROOT");
        assert_eq!(bent([5; 16], [4; 16]), stray("volume", Stray::OtherDisk), "another disk's");
        assert_eq!(bent([9; 16], [4; 16]), stray("volume", Stray::Unlisted));
        let mut twice = listed.to_vec();
        twice.push(at(1, KINDS.boot, [3; 16]));
        assert_eq!(grant(&t, &running, &twice, KINDS), stray("volume", Stray::Duplicate), "a GUID two disks carry");
    }
}
