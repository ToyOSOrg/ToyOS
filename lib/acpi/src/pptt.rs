//! The PPTT (signature `PPTT`): ACPI 6.5 §5.2.30 — which core, last-level
//! cache and package each processor sits in.
//!
//! [`Pptt::open`] walks every structure once, refusing one the table cannot
//! hold; [`Pptt::place`] answers one processor by following parent references
//! a bounded number of times, refusing any reference that does not name a
//! processor node inside the table.

use crate::{find_table, Phys, Table, TableError, SDT_HEADER_LEN, SDT_REVISION};

/// Revision 2 is ACPI 6.3's, the first with the leaf and thread flags.
const FIRST_REVISION: u8 = 2;

/// §5.2.30.1: type (0), length (1), reserved (2..4), flags (4..8), parent
/// (8..12), ACPI processor ID (12..16), private resource count (16..20),
/// the resources from 20.
const NODE: u8 = 0;
const NODE_LEN: usize = 20;
const NODE_FLAGS: usize = 4;
const NODE_PARENT: usize = 8;
const NODE_ID: usize = 12;
const NODE_RESOURCES: usize = 16;
const PHYSICAL_PACKAGE: u32 = 1 << 0;
const ID_VALID: u32 = 1 << 1;
const THREAD: u32 = 1 << 2;
const LEAF: u32 = 1 << 3;

/// §5.2.30.2: a cache type structure is 24 bytes up to revision 2 and 28 from
/// revision 3, which adds the cache ID; attributes at 21, bits 3:2 its type.
const CACHE: u8 = 1;
const CACHE_LEN: usize = 24;
const CACHE_ATTRIBUTES: usize = 21;
const INSTRUCTION: u8 = 1;

/// Why a PPTT, or one processor in it, cannot be placed. `at` is a
/// structure's offset in the table.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PpttRefused {
    Table(TableError),
    /// A revision before the leaf and thread flags.
    Revision(u8),
    /// A structure shorter than its header or its type, or past the table.
    Structure { at: usize, declared: usize },
    /// No leaf node carries this processor's ACPI processor ID.
    NoLeaf { uid: u32 },
    /// A parent reference that names no processor node.
    Parent { at: usize, parent: u32 },
    /// The ancestors of this processor never reach a root: a cycle.
    Cycle { uid: u32 },
    /// No node from this processor up is a physical package.
    NoPackage { uid: u32 },
}

impl From<TableError> for PpttRefused {
    fn from(error: TableError) -> Self {
        Self::Table(error)
    }
}

/// One processor's nodes, each named by its offset in the table, which is
/// unique to it: its core, the node owning the last-level cache it reaches,
/// and its package. A table that describes no shared cache puts the LLC at
/// the package, the only shared domain it names.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Place {
    pub core: u32,
    pub llc: u32,
    pub package: u32,
}

#[derive(Clone, Copy)]
pub struct Pptt<P> {
    table: Table<P>,
    /// Structures in the table, which bounds every walk up a tree.
    structures: usize,
}

/// The PPTT at `rsdp_addr`, walked; `Table(Absent)` where firmware has none.
pub fn pptt<P: Phys>(phys: P, rsdp_addr: u64) -> Result<Pptt<P>, PpttRefused> {
    Pptt::open(find_table(phys, rsdp_addr, b"PPTT", SDT_HEADER_LEN)?)
}

impl<P: Phys> Pptt<P> {
    pub fn open(table: Table<P>) -> Result<Self, PpttRefused> {
        let revision = table.byte(SDT_REVISION).unwrap_or(0);
        if revision < FIRST_REVISION {
            return Err(PpttRefused::Revision(revision));
        }
        let pptt = Self { table, structures: 0 };
        let mut structures = 0;
        let mut at = SDT_HEADER_LEN;
        while at < table.len() {
            let (_, declared) = pptt.structure(at).ok_or(PpttRefused::Structure {
                at,
                declared: table.byte(at + 1).map_or(0, usize::from),
            })?;
            structures += 1;
            at += declared;
        }
        Ok(Self { table, structures })
    }

    /// The type and length of a structure at `at` whose declared length holds
    /// its type's fields inside the table.
    fn structure(&self, at: usize) -> Option<(u8, usize)> {
        let kind = self.table.byte(at)?;
        let declared = usize::from(self.table.byte(at + 1)?);
        let floor = match kind {
            NODE => (self.table.u32_at(at + NODE_RESOURCES)? as usize).checked_mul(4)?.checked_add(NODE_LEN)?,
            CACHE => CACHE_LEN,
            _ => 2,
        };
        (at >= SDT_HEADER_LEN && declared >= floor && at.checked_add(declared)? <= self.table.len())
            .then_some((kind, declared))
    }

    /// A word of a structure [`Self::structure`] has accepted.
    fn word(&self, at: usize) -> u32 {
        self.table.u32_at(at).expect("an accepted structure lies inside the table")
    }

    /// Whether the node at `at` lists a data or unified cache as its own.
    fn owns_a_cache(&self, at: usize) -> bool {
        let count = self.word(at + NODE_RESOURCES) as usize;
        (0..count).any(|i| {
            let cache = self.word(at + NODE_LEN + 4 * i) as usize;
            self.structure(cache).is_some_and(|(kind, _)| kind == CACHE)
                && self.table.byte(cache + CACHE_ATTRIBUTES).is_some_and(|attributes| (attributes >> 2) & 3 != INSTRUCTION)
        })
    }

    /// Where the processor whose ACPI processor ID is `uid` sits: the MADT
    /// GICC's `ACPI Processor UID` names it.
    pub fn place(&self, uid: u32) -> Result<Place, PpttRefused> {
        let leaf = (0..self.structures)
            .scan(SDT_HEADER_LEN, |at, _| {
                let here = *at;
                *at += usize::from(self.table.byte(here + 1)?);
                Some(here)
            })
            .find(|&at| {
                self.structure(at).is_some_and(|(kind, _)| kind == NODE)
                    && self.word(at + NODE_FLAGS) & (LEAF | ID_VALID) == LEAF | ID_VALID
                    && self.word(at + NODE_ID) == uid
            })
            .ok_or(PpttRefused::NoLeaf { uid })?;
        let parent = |at: usize| -> Result<Option<usize>, PpttRefused> {
            match self.word(at + NODE_PARENT) {
                0 => Ok(None),
                reference => match self.structure(reference as usize) {
                    Some((NODE, _)) => Ok(Some(reference as usize)),
                    _ => Err(PpttRefused::Parent { at, parent: reference }),
                },
            }
        };
        let core = if self.word(leaf + NODE_FLAGS) & THREAD != 0 {
            parent(leaf)?.ok_or(PpttRefused::Parent { at: leaf, parent: 0 })?
        } else {
            leaf
        };
        let (mut llc, mut package) = (None, None);
        let mut node = Some(leaf);
        for _ in 0..=self.structures {
            let Some(at) = node else {
                let package = package.ok_or(PpttRefused::NoPackage { uid })?;
                return Ok(Place { core: core as u32, llc: llc.unwrap_or(package) as u32, package: package as u32 });
            };
            if package.is_none() {
                if self.owns_a_cache(at) {
                    llc = Some(at);
                }
                if self.word(at + NODE_FLAGS) & PHYSICAL_PACKAGE != 0 {
                    package = Some(at);
                }
            }
            node = parent(at)?;
        }
        Err(PpttRefused::Cycle { uid })
    }
}
