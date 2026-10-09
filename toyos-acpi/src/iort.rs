//! The IORT (signature `IORT`): Arm's IO Remapping Table, DEN 0049 issue E.g
//! — which requester reaches which SMMUv3 under which StreamID, and which
//! DeviceID its messages carry to which ITS.
//!
//! [`iort`] answers a table every node of which it has walked: a node or a
//! mapping array the table cannot hold is its refusal, and so is a revision
//! whose layout was not read against the specification. An output reference
//! is followed, and judged, by the [`Iort::route`] that needs it: opening the
//! table and routing one requester each read it a bounded number of times,
//! whatever its nodes and mappings say.
//!
//! Only the three node types that route a PCI function are decoded — the ITS,
//! the root complex and the SMMUv3; any other is answered by its type alone,
//! for its caller to rule on, and is refused as the output of a mapping.

use core::num::NonZeroU32;

use crate::{find_table, Phys, Table, TableError, SDT_HEADER_LEN, SDT_REVISION};

/// Table 3: `Number of IORT Nodes` at 36, `Offset to Array of IORT Nodes` at
/// 40, one reserved word, and the node array no earlier than that.
const NODE_COUNT: usize = SDT_HEADER_LEN;
const NODE_ARRAY: usize = SDT_HEADER_LEN + 4;
pub const IORT_NEEDED: usize = SDT_HEADER_LEN + 12;

/// The table revisions decoded: 5 is issue E.d's, 6 issues E.e's and E.f's and
/// 7 issue E.g's. Each carries a revision in every node, which is what a
/// node's own layout is judged by.
const REVISIONS: core::ops::RangeInclusive<u8> = 5..=7;

/// Table 4: type (0), length (1..3), revision (3), identifier (4..8), number
/// of ID mappings (8..12), reference to the ID array (12..16).
const NODE_HEADER: usize = 16;
/// Table 5: input base, number of IDs minus one, output base, output
/// reference, flags — five words.
const MAPPING: usize = 20;
/// Table 6: bit 0, "apply the output base regardless of the input IDs".
const SINGLE: u32 = 1 << 0;

const ITS: u8 = 0;
const ROOT_COMPLEX: u8 = 2;
const SMMUV3: u8 = 4;

/// Table 16: one reserved word that counted the group's ITSs before issue
/// E.g and is 1 since, then the one identifier.
const ITS_LEN: usize = NODE_HEADER + 8;
/// Table 18: memory access properties (16..24), ATS attribute (24..28), PCI
/// segment number (28..32), then the address size limit and, from node
/// revision 4, the PASID capabilities and flags.
const RC_SEGMENT: usize = 28;
/// Table 13: base address (16..24), flags (24..28), a reserved word, VATOS
/// (32..40), model (40..44), the event, PRI, GERR and sync GSIVs (44..60),
/// proximity domain (60..64), DeviceID mapping index (64..68).
const SMMU_BASE: usize = 16;
const SMMU_FLAGS: usize = 24;
const SMMU_GSIVS: usize = 44;
const SMMU_INDEX: usize = 64;
const SMMU_LEN: usize = 68;
/// Table 14, bit 0: `COHACC override`.
const SMMU_COHACC: u32 = 1 << 0;
/// Table 14, bit 4, from node revision 5: `DeviceID mapping index valid`.
const SMMU_INDEXED: u32 = 1 << 4;

/// Why an IORT cannot be used. `at` is a node's offset in the table.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IortRefused {
    Table(TableError),
    /// A table revision outside the ones decoded.
    Revision(u8),
    /// The node array starts inside the table's own header fields.
    NodeArray { offset: u32 },
    /// The node at `at` declares a length too short for its type, or one the
    /// table cannot hold; a node count that overruns the table ends here.
    Node { at: usize, declared: usize },
    /// A node revision whose layout was not read against the specification.
    NodeRevision { at: usize, kind: u8, revision: u8 },
    /// The node's ID array lies over its own fields or runs past its end, or
    /// an ITS node, which has none, declares one.
    Mappings { at: usize, count: u32, offset: u32 },
    /// A mapping's input or output range runs past the 32-bit ID space.
    Range { at: usize, base: u32, last: u32 },
    /// The mapping a requester is routed by names no node with its output
    /// reference, or a node its source may not output to: a root complex
    /// outputs to an SMMUv3 or an ITS, an SMMUv3 to an ITS.
    Output { at: usize, reference: u32 },
    /// A mapping with the single-mapping flag that is not its SMMUv3's own.
    /// Table 6 allows one in a root complex and in an SMMUv3, where it puts
    /// every input ID out as one; nothing here routes that.
    SingleMapping { at: usize },
    /// An ITS group of other than one ITS, which a table of revision 5 or 6
    /// may hold: issue E.g fixes the count at 1, and nothing here routes to
    /// a group.
    ItsGroup { at: usize, count: u32 },
    /// Two mappings claim this ID, which the table must route one way.
    Overlap { id: u32 },
}

impl From<TableError> for IortRefused {
    fn from(error: TableError) -> Self {
        Self::Table(error)
    }
}

/// An SMMUv3 as its node names it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Smmuv3 {
    pub base: u64,
    /// Table 14's `COHACC override`: set, the unit reads its tables and
    /// queues coherently whatever its `SMMU_IDR0.COHACC` says.
    pub coherent_override: bool,
    /// The wired interrupts; `None` where the node gives no GSIV.
    pub event: Option<NonZeroU32>,
    pub gerror: Option<NonZeroU32>,
    pub sync: Option<NonZeroU32>,
}

/// One node, in the shape its reader acts on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Node {
    /// `id` is the MADT GIC ITS structure's.
    Its { id: u32 },
    RootComplex { segment: u32 },
    Smmuv3(Smmuv3),
    /// A type not decoded here: a named component (1), an SMMUv1 or v2 (3), a
    /// PMCG (5), a reserved memory range (6), an IWB (7), or a reserved one.
    Other(u8),
}

/// The DeviceID a requester's messages carry, and the ITS they carry it to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ItsDevice {
    pub its: u32,
    pub device: u32,
}

/// Where one requester's transactions go.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Route {
    /// Through this SMMUv3 under `stream`; `its` is where the SMMU's own node
    /// maps that stream on to, and `None` where it maps it nowhere. The
    /// unit's own DeviceID (Table 13, `DeviceID mapping index`) is no
    /// stream's.
    Translated { smmu: Smmuv3, stream: u32, its: Option<ItsDevice> },
    /// Straight to an ITS, past every SMMU.
    Untranslated(ItsDevice),
    /// No root complex of that segment maps the requester.
    Unmapped,
}

/// A node's header, bounded by the table.
#[derive(Clone, Copy)]
struct Raw {
    at: usize,
    kind: u8,
    len: usize,
    revision: u8,
    mappings: u32,
    array: u32,
}

#[derive(Clone, Copy)]
struct Mapping {
    input: u32,
    /// The number of IDs, minus one.
    span: u32,
    output: u32,
    reference: u32,
    single: bool,
}

impl Mapping {
    fn maps(&self, id: u32) -> Option<u32> {
        // `output + span` was checked not to wrap when the table was opened.
        (id >= self.input && id - self.input <= self.span).then(|| self.output + (id - self.input))
    }
}

/// An IORT whose every node [`iort`] has walked.
#[derive(Clone, Copy)]
pub struct Iort<P> {
    table: Table<P>,
    nodes: u32,
    array: usize,
}

/// The IORT at `rsdp_addr`, every node of it checked.
pub fn iort<P: Phys>(phys: P, rsdp_addr: u64) -> Result<Iort<P>, IortRefused> {
    let table = find_table(phys, rsdp_addr, b"IORT", IORT_NEEDED)?;
    let short = TableError::Length { declared: table.len() as u32, needed: IORT_NEEDED };
    let revision = table.byte(SDT_REVISION).ok_or(short)?;
    if !REVISIONS.contains(&revision) {
        return Err(IortRefused::Revision(revision));
    }
    let nodes = table.u32_at(NODE_COUNT).ok_or(short)?;
    let offset = table.u32_at(NODE_ARRAY).ok_or(short)?;
    if (offset as usize) < IORT_NEEDED {
        return Err(IortRefused::NodeArray { offset });
    }
    let iort = Iort { table, nodes, array: offset as usize };
    for node in iort.raw_nodes() {
        iort.check(node?)?;
    }
    Ok(iort)
}

impl<P: Phys> Iort<P> {
    /// Every node, in the table's order.
    pub fn nodes(&self) -> impl Iterator<Item = Node> + '_ {
        // `iort` walked the same headers and decoded the same nodes.
        self.raw_nodes().filter_map(|raw| self.decode(raw.ok()?).ok())
    }

    /// Where the PCI function `rid` of `segment` is routed.
    pub fn route(&self, segment: u32, rid: u16) -> Result<Route, IortRefused> {
        let rid = u32::from(rid);
        let mut hit = None;
        for raw in self.raw_nodes() {
            let raw = raw?;
            if self.decode(raw)? != (Node::RootComplex { segment }) {
                continue;
            }
            for found in self.hits(raw, rid) {
                if hit.replace((raw, found)).is_some() {
                    return Err(IortRefused::Overlap { id: rid });
                }
            }
        }
        let Some((root, (mapping, id))) = hit else {
            return Ok(Route::Unmapped);
        };
        match self.output(root, mapping)? {
            (_, Node::Its { id: its }) => Ok(Route::Untranslated(ItsDevice { its, device: id })),
            (unit, Node::Smmuv3(smmu)) => {
                let mut onward = None;
                for found in self.hits(unit, id) {
                    if onward.replace(found).is_some() {
                        return Err(IortRefused::Overlap { id });
                    }
                }
                let its = match onward {
                    None => None,
                    Some((mapping, device)) => match self.output(unit, mapping)? {
                        (_, Node::Its { id: its }) => Some(ItsDevice { its, device }),
                        _ => return Err(IortRefused::Output { at: unit.at, reference: mapping.reference }),
                    },
                };
                Ok(Route::Translated { smmu, stream: id, its })
            }
            _ => Err(IortRefused::Output { at: root.at, reference: mapping.reference }),
        }
    }

    /// The index of the mapping that is an SMMUv3's own DeviceID (Table 13,
    /// offset 64), where its node says it has one: by the flag from node
    /// revision 5, and before it wherever a control interrupt has no GSIV.
    /// The entry's input base and length are not read.
    fn own(&self, raw: Raw) -> Option<u32> {
        let t = &self.table;
        if raw.kind != SMMUV3 {
            return None;
        }
        let indexed = if raw.revision >= 5 {
            t.u32_at(raw.at + SMMU_FLAGS)? & SMMU_INDEXED != 0
        } else {
            (0..4).any(|i| t.u32_at(raw.at + SMMU_GSIVS + 4 * i) == Some(0))
        };
        if indexed {
            t.u32_at(raw.at + SMMU_INDEX)
        } else {
            None
        }
    }

    /// The mappings of `raw` that hold `id`, each with the ID it puts out.
    fn hits(&self, raw: Raw, id: u32) -> impl Iterator<Item = (Mapping, u32)> + '_ {
        let own = self.own(raw);
        (0..raw.mappings).filter(move |index| Some(*index) != own).filter_map(move |index| {
            let mapping = self.mapping(raw, index)?;
            Some((mapping, mapping.maps(id)?))
        })
    }

    /// Walks by each node's own length, which [`Iort::raw`] bounds below by
    /// the header and above by the table: at most `len / 16` steps.
    fn raw_nodes(&self) -> impl Iterator<Item = Result<Raw, IortRefused>> + '_ {
        let mut at = self.array;
        let mut halted = false;
        (0..self.nodes).map_while(move |_| {
            if halted {
                return None;
            }
            let raw = self.raw(at);
            match raw {
                Ok(raw) => at += raw.len,
                Err(_) => halted = true,
            }
            Some(raw)
        })
    }

    fn raw(&self, at: usize) -> Result<Raw, IortRefused> {
        let t = &self.table;
        let header = || Some((t.byte(at)?, t.u16_at(at + 1)?, t.byte(at + 3)?, t.u32_at(at + 8)?, t.u32_at(at + 12)?));
        let Some((kind, declared, revision, mappings, array)) = header() else {
            return Err(IortRefused::Node { at, declared: 0 });
        };
        let len = usize::from(declared);
        let floor = match kind {
            ITS => ITS_LEN,
            ROOT_COMPLEX => RC_SEGMENT + 4,
            SMMUV3 => SMMU_LEN,
            _ => NODE_HEADER,
        };
        if len < floor || at + len > t.len() {
            return Err(IortRefused::Node { at, declared: len });
        }
        Ok(Raw { at, kind, len, revision, mappings, array })
    }

    /// Mapping `index` of a node [`Iort::check`] accepted the array of.
    fn mapping(&self, raw: Raw, index: u32) -> Option<Mapping> {
        let at = raw.at + raw.array as usize + index as usize * MAPPING;
        let t = &self.table;
        Some(Mapping {
            input: t.u32_at(at)?,
            span: t.u32_at(at + 4)?,
            output: t.u32_at(at + 8)?,
            reference: t.u32_at(at + 12)?,
            single: t.u32_at(at + 16)? & SINGLE != 0,
        })
    }

    /// The node a mapping of `from` outputs to: one walk of the nodes.
    fn output(&self, from: Raw, mapping: Mapping) -> Result<(Raw, Node), IortRefused> {
        let refused = IortRefused::Output { at: from.at, reference: mapping.reference };
        for raw in self.raw_nodes() {
            let raw = raw?;
            if raw.at == mapping.reference as usize {
                return Ok((raw, self.decode(raw)?));
            }
        }
        Err(refused)
    }

    fn decode(&self, raw: Raw) -> Result<Node, IortRefused> {
        let t = &self.table;
        let Raw { at, kind, revision, .. } = raw;
        let short = IortRefused::Node { at, declared: raw.len };
        let revised = |known: core::ops::RangeInclusive<u8>| {
            if known.contains(&revision) {
                Ok(())
            } else {
                Err(IortRefused::NodeRevision { at, kind, revision })
            }
        };
        Ok(match kind {
            ITS => {
                revised(1..=1)?;
                let count = t.u32_at(at + NODE_HEADER).ok_or(short)?;
                if count != 1 {
                    return Err(IortRefused::ItsGroup { at, count });
                }
                Node::Its { id: t.u32_at(at + NODE_HEADER + 4).ok_or(short)? }
            }
            ROOT_COMPLEX => {
                revised(3..=4)?;
                Node::RootComplex { segment: t.u32_at(at + RC_SEGMENT).ok_or(short)? }
            }
            SMMUV3 => {
                revised(4..=5)?;
                // Event, PRI, GERR, sync: the PRI queue is not used, and its GSIV not answered.
                let gsiv = |i: usize| t.u32_at(at + SMMU_GSIVS + 4 * i).map(NonZeroU32::new).ok_or(short);
                Node::Smmuv3(Smmuv3 {
                    base: t.u64_at(at + SMMU_BASE).ok_or(short)?,
                    coherent_override: t.u32_at(at + SMMU_FLAGS).ok_or(short)? & SMMU_COHACC != 0,
                    event: gsiv(0)?,
                    gerror: gsiv(2)?,
                    sync: gsiv(3)?,
                })
            }
            other => Node::Other(other),
        })
    }

    /// One node's fields, its ID array's place and every mapping's own words.
    fn check(&self, raw: Raw) -> Result<(), IortRefused> {
        let node = self.decode(raw)?;
        // Where the node's own fields end, which its ID array may not lie over.
        let fields = match (node, raw.revision) {
            (Node::Other(_), _) => return Ok(()),
            (Node::Its { .. }, _) => ITS_LEN,
            (Node::RootComplex { .. }, 3) => 36,
            (Node::RootComplex { .. }, _) => 40,
            (Node::Smmuv3(_), _) => SMMU_LEN,
        };
        let Raw { at, mappings: count, array: offset, .. } = raw;
        let end = u64::from(offset) + u64::from(count) * MAPPING as u64;
        let misplaced = (offset as usize) < fields || end > raw.len as u64;
        if count != 0 && (misplaced || matches!(node, Node::Its { .. })) {
            return Err(IortRefused::Mappings { at, count, offset });
        }
        let own = self.own(raw);
        for index in (0..count).filter(|index| Some(*index) != own) {
            let mapping = self.mapping(raw, index).ok_or(IortRefused::Mappings { at, count, offset })?;
            if mapping.single {
                return Err(IortRefused::SingleMapping { at });
            }
            for base in [mapping.input, mapping.output] {
                if base.checked_add(mapping.span).is_none() {
                    return Err(IortRefused::Range { at, base, last: mapping.span });
                }
            }
        }
        Ok(())
    }
}
