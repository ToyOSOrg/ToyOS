//! The stage 1 tables a context descriptor names: VMSAv8-64 with the 4 KiB
//! granule over a 48-bit input, four levels of 512 descriptors (Arm ARM,
//! DDI 0487 M.d, §D8.3.1), and the plan of one mapping in them.
//!
//! A device's memory is mapped by 2 MiB blocks at level 2. The page an ITS
//! takes a message on is the one 4 KiB page at level 3: a block there would
//! hand the device every register that shares its 2 MiB.

use crate::Phys;

/// The width of a device address these tables translate.
pub const INPUT_BITS: u32 = 48;
/// Levels 0 to 3, each indexed by nine bits of the address.
pub const LEVELS: usize = 4;

/// `CD.MAIR0`: attribute 0 is Device-nGnRE (`0x04`), which the message page
/// is mapped with, and attribute 1 Normal write-back memory (`0xFF`).
pub const MAIR0: u32 = 0x04 | 0xFF << 8;
const ATTR_DEVICE: u64 = 0 << 2;
const ATTR_MEMORY: u64 = 1 << 2;

/// Table D8-50 and Table D8-52: bit [0] valid; bit [1] set for a table
/// descriptor and a level 3 page, clear for a block.
const VALID: u64 = 1 << 0;
const TABLE_OR_PAGE: u64 = 1 << 1;
/// `AP[1]`, bit [6]: an unprivileged access is permitted. A PCIe transaction
/// without a PASID prefix is unprivileged (IHI 0070 §13.7), so a leaf without
/// it is one no device reaches.
const AP_UNPRIVILEGED: u64 = 1 << 6;
/// `AP[2]`, bit [7]: read-only.
const AP_READ_ONLY: u64 = 1 << 7;
/// `SH`, bits [9:8]: inner shareable.
const INNER_SHAREABLE: u64 = 0b11 << 8;
/// The access flag, bit [10]: clear, the first access faults.
const AF: u64 = 1 << 10;
/// `nG`, bit [11]: the translation is its context's alone. A leaf without it
/// is global, cached for every ASID of the regime and so for every domain,
/// and no invalidation by ASID removes it.
const NOT_GLOBAL: u64 = 1 << 11;
/// `PXN` [53] and `UXN` [54]: never executable.
const NEVER_EXECUTED: u64 = 1 << 53 | 1 << 54;

const LEAF: u64 = VALID | AP_UNPRIVILEGED | AF | NOT_GLOBAL | NEVER_EXECUTED;

/// What a device may do to memory mapped for it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Access {
    Read,
    ReadWrite,
}

/// What one mapping puts at a device address.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Leaf {
    /// 2 MiB of memory.
    Memory(Phys<21>, Access),
    /// The 4 KiB page holding the register a message is written to.
    Doorbell(Phys<12>),
}

/// The descriptor naming the next table down, at levels 0 to 2. Its
/// hierarchical fields stay clear: the leaf alone decides the access.
pub const fn next(table: Phys<12>) -> u64 {
    table.get() | TABLE_OR_PAGE | VALID
}

/// One mapping, planned: the index to follow in each table from the root,
/// and the descriptor to store at the last of them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Path {
    indices: [usize; LEVELS],
    tables: usize,
    /// The leaf: a level 2 block, or a level 3 page.
    pub descriptor: u64,
}

impl Path {
    /// The index in the table at each level walked, the root first; the
    /// last is the leaf's own slot, and every one before it holds a
    /// [`next`] descriptor.
    pub fn indices(&self) -> &[usize] {
        &self.indices[..self.tables]
    }
}

/// The path to `leaf` at the device address `at`. `None` where `at` is past
/// the input or not aligned to what the leaf maps.
pub const fn plan(at: u64, leaf: Leaf) -> Option<Path> {
    let (tables, size, descriptor) = match leaf {
        Leaf::Memory(block, access) => {
            let access = match access {
                Access::Read => AP_READ_ONLY,
                Access::ReadWrite => 0,
            };
            (3, 1u64 << 21, block.get() | LEAF | ATTR_MEMORY | INNER_SHAREABLE | access)
        }
        Leaf::Doorbell(page) => (4, 1u64 << 12, page.get() | LEAF | TABLE_OR_PAGE | ATTR_DEVICE),
    };
    if at >> INPUT_BITS != 0 || at & (size - 1) != 0 {
        return None;
    }
    const fn index(at: u64, level: u32) -> usize {
        (at >> (39 - 9 * level) & 0x1FF) as usize
    }
    Some(Path { indices: [index(at, 0), index(at, 1), index(at, 2), index(at, 3)], tables, descriptor })
}

/// What a descriptor read back from a table is, by the level it was read at.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Entry {
    /// Nothing is mapped under it.
    Invalid,
    /// The next table down.
    Table(Phys<12>),
    /// A block or a page: something is mapped here.
    Mapped,
}

/// `descriptor` as the walk reads it at `level`: bit [1] names a table above
/// level 3 and a page at it, and a table's address is bits [47:12].
pub const fn entry(descriptor: u64, level: usize) -> Entry {
    if descriptor & VALID == 0 {
        Entry::Invalid
    } else if level < LEVELS - 1 && descriptor & TABLE_OR_PAGE != 0 {
        Entry::Table(Phys(descriptor & 0x0000_FFFF_FFFF_F000))
    } else {
        Entry::Mapped
    }
}
