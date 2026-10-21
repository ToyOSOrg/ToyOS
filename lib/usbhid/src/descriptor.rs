//! A HID report descriptor (HID 1.11 §6.2.2), read into the input fields of
//! each top-level application collection.
//!
//! **Every byte is the device's**, so every length and value is bounded before
//! it is used, and nothing here panics or loops on a device's word. What
//! breaks the reading itself — an item running past the end, a collection
//! closed that was never opened or never closed, a Push past the stack, Report
//! ID 0, a main item outside every collection — refuses the interface by name
//! ([`Refused`]).
//!
//! **A report belongs to one top-level collection** (§8.4: "reports may not
//! span more than one top level collection"), so what this reader cannot
//! bind costs only its own collection, whose reports are the Report IDs its
//! fields carry (§6.2.2.7): [`Unbound`]. A Report ID two top-level collections
//! both carry is a descriptor §8.4 forbids, and binding one half would read
//! the other's fields as absent: the interface is refused.

use alloc::vec::Vec;

/// The longest input report, its Report ID byte included, this reader binds:
/// past it the collection is not bound. A full-speed interrupt endpoint moves
/// at most 64 bytes a packet (USB 2.0 §5.7.3), so a longer report spans
/// transfers.
pub const MAX_REPORT_BYTES: usize = 64;

/// HID Usage Tables 1.5's pages and the usages this crate binds: Generic
/// Desktop (§4), Keyboard/Keypad (§10), Button (§12).
pub const GENERIC_DESKTOP: u16 = 0x01;
pub const KEYS: u16 = 0x07;
pub const BUTTON: u16 = 0x09;
pub const POINTER: u16 = 0x01;
pub const MOUSE: u16 = 0x02;
pub const KEYBOARD: u16 = 0x06;
pub const KEYPAD: u16 = 0x07;
pub const X: u16 = 0x30;
pub const Y: u16 = 0x31;
pub const WHEEL: u16 = 0x38;

/// Push's depth; no descriptor in reach pushes more than twice.
const GLOBAL_DEPTH: usize = 4;
/// Collections nested inside one another.
const NESTING: usize = 8;
/// Usages one main item's locals may list one by one.
const LISTED: usize = 64;
/// Top-level collections one interface may declare.
const COLLECTIONS: usize = 16;
/// How far an array's usage list is walked to say what it names: past the
/// whole Keyboard/Keypad page.
const LIST_WALK: i32 = 0x100;

/// Why an interface's descriptor is not read at all.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refused {
    /// An item whose data runs past the descriptor's end.
    Truncated { at: usize },
    /// An End Collection with no collection open, or a collection never closed.
    Unbalanced { at: usize },
    /// Collections nested deeper than this reader follows, or more top-level
    /// ones than it keeps.
    TooDeep { at: usize },
    /// A Push past the global stack, or a Pop of an empty one.
    GlobalStack { at: usize },
    /// A main item outside every collection.
    Outside { at: usize },
    /// Report ID 0, which §6.2.2.7 reserves, or one past a byte.
    ReportId { at: usize },
    /// A field with no Report ID in a descriptor that numbers its reports:
    /// every report then begins with its ID (§6.2.2.7).
    Unnumbered,
    /// A Report ID two top-level collections both carry (§8.4).
    SpansCollections { id: u8 },
}

/// Why one collection is not bound: its reports are counted and dropped.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unbound {
    /// Neither a keyboard's keys nor a pointer's axes: a consumer control, a
    /// vendor's collection, a UPS.
    NotAKeyboardOrPointer { page: u16, usage: u16 },
    /// An input report longer than [`MAX_REPORT_BYTES`].
    ReportTooLong { id: u8 },
    /// A Report Size of 0 or past 32 bits.
    ReportSize { bits: u32 },
    /// More usages listed one by one than this reader keeps.
    TooManyUsages,
    /// An axis or a key field whose Logical Maximum is not above its Logical
    /// Minimum: nothing maps its values.
    EmptyRange { page: u16, usage: u16 },
}

/// The usages one field's controls name, in control order (§6.2.2.8).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Usages {
    /// Usage Minimum to Usage Maximum.
    Range { first: u32, last: u32 },
    /// Usage items one by one.
    Listed(Vec<u32>),
}

impl Usages {
    /// Entry `index` of the list, or `None` past its end. An extended usage
    /// carries its page in its top half.
    pub fn at(&self, index: u32) -> Option<u32> {
        match self {
            Self::Range { first, last } => first.checked_add(index).filter(|u| u <= last),
            Self::Listed(list) => list.get(index as usize).copied(),
        }
    }
}

/// One Input main item's controls.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Field {
    /// Its report's ID, 0 where the interface numbers none.
    pub report: u8,
    /// Where its first control starts, in bits past the report's ID byte.
    pub bit: u32,
    pub size: u32,
    pub count: u32,
    /// Data, not constant padding.
    pub data: bool,
    /// A variable field's controls are one usage each; an array field's are
    /// indexes into its usages (§6.2.2.5).
    pub variable: bool,
    pub relative: bool,
    pub page: u16,
    pub usages: Usages,
    pub logical_min: i32,
    pub logical_max: i32,
}

impl Field {
    /// The page and usage control `index` of a variable field names. Past a
    /// list given one by one, the last usage repeats for every control
    /// (§6.2.2.8).
    pub fn control(&self, index: u32) -> Option<(u16, u16)> {
        let usage = match &self.usages {
            Usages::Range { .. } => self.usages.at(index),
            Usages::Listed(list) => list.get(index as usize).or(list.last()).copied(),
        };
        usage.map(|u| self.page_of(u))
    }

    /// What an array field's control value `value` selects: `None` for a value
    /// outside Logical Minimum to Maximum, which asserts nothing (§6.2.2.5),
    /// `Some(None)` for an index past the field's usage list, and otherwise
    /// the usage at index `value - Logical Minimum`.
    pub fn selected(&self, value: i32) -> Option<Option<(u16, u16)>> {
        if !self.holds(value) {
            return None;
        }
        let index = (i64::from(value) - i64::from(self.logical_min)) as u32;
        Some(self.usages.at(index).map(|u| self.page_of(u)))
    }

    /// Whether `value` is inside Logical Minimum to Maximum.
    pub fn holds(&self, value: i32) -> bool {
        (self.logical_min..=self.logical_max).contains(&value)
    }

    /// Control `index`'s value in `report`, the bytes past its ID: signed
    /// where Logical Minimum is negative (§6.2.2.7), or `None` for a report
    /// too short to hold it.
    pub fn value(&self, report: &[u8], index: u32) -> Option<i32> {
        let first = u64::from(self.bit) + u64::from(index) * u64::from(self.size);
        let mut raw = 0u64;
        for n in 0..u64::from(self.size) {
            let at = first + n;
            let byte = *report.get(usize::try_from(at / 8).ok()?)?;
            raw |= u64::from((byte >> (at % 8)) & 1) << n;
        }
        if self.logical_min < 0 && self.size > 0 && raw >> (self.size - 1) & 1 == 1 {
            raw |= u64::MAX << self.size;
        }
        Some(raw as i32)
    }

    /// The bits the field's controls end at, past the report's ID byte.
    pub fn end(&self) -> u64 {
        u64::from(self.bit) + u64::from(self.size) * u64::from(self.count)
    }

    fn page_of(&self, usage: u32) -> (u16, u16) {
        match usage >> 16 {
            0 => (self.page, usage as u16),
            page => (page as u16, usage as u16),
        }
    }

    /// Whether any control of this field names `page`'s `usage`.
    fn names(&self, page: u16, usage: u16) -> bool {
        if self.variable {
            (0..self.count).any(|i| self.control(i) == Some((page, usage)))
        } else {
            (0..=self.logical_max.saturating_sub(self.logical_min).clamp(0, LIST_WALK))
                .any(|i| self.usages.at(i as u32).map(|u| self.page_of(u)) == Some((page, usage)))
        }
    }

    /// Whether this field reports keys: a Keyboard/Keypad page usage.
    pub fn keys(&self) -> bool {
        self.page == KEYS || self.usages.at(0).is_some_and(|u| u >> 16 == u32::from(KEYS))
    }
}

/// What a bound collection is to the owner of input.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Keyboard,
    Pointer,
}

/// One top-level application collection, and every input field inside it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Collection {
    pub page: u16,
    pub usage: u16,
    /// Report IDs its fields carry; empty where the interface numbers none.
    pub reports: Vec<u8>,
    pub fields: Vec<Field>,
    pub bound: Result<Kind, Unbound>,
}

/// A whole interface's descriptor.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Interface {
    pub collections: Vec<Collection>,
    /// Whether every report begins with its Report ID.
    pub numbered: bool,
}

impl Interface {
    /// The collection report `id` belongs to; for an interface that numbers
    /// none, its one collection with input fields.
    pub fn of(&self, id: u8) -> Option<usize> {
        self.collections
            .iter()
            .position(|c| if self.numbered { c.reports.contains(&id) } else { !c.fields.is_empty() })
    }
}

#[derive(Clone, Copy, Default)]
struct Globals {
    page: u16,
    logical_min: i32,
    logical_max: i32,
    size: u32,
    count: u32,
    report: u8,
}

#[derive(Default)]
struct Locals {
    listed: Vec<u32>,
    min: Option<u32>,
    max: Option<u32>,
    too_many: bool,
}

/// A 4-byte usage is extended and keeps its page; a shorter one is on the
/// Usage Page in force (§6.2.2.8).
fn usage(len: usize, unsigned: u32) -> u32 {
    if len == 4 { unsigned } else { unsigned & 0xFFFF }
}

/// Read `bytes`, the descriptor a HID interface answered
/// GET_DESCRIPTOR(Report) with.
pub fn parse(bytes: &[u8]) -> Result<Interface, Refused> {
    let mut globals = Globals::default();
    let mut stack: Vec<Globals> = Vec::new();
    let mut locals = Locals::default();
    let mut depth = 0usize;
    let mut collections: Vec<Collection> = Vec::new();
    // What stops each collection binding, found while it was read.
    let mut flaws: Vec<Option<Unbound>> = Vec::new();
    // Input bits so far of each report, by ID.
    let mut lengths: Vec<(u8, u32)> = Vec::new();
    let mut numbered = false;
    let mut at = 0usize;
    while at < bytes.len() {
        let prefix = bytes[at];
        // A long item (§6.2.2.3): no tag is defined, so it is stepped over.
        if prefix == 0xFE {
            let len = usize::from(*bytes.get(at + 1).ok_or(Refused::Truncated { at })?);
            at = Some(at + 3 + len).filter(|end| *end <= bytes.len()).ok_or(Refused::Truncated { at })?;
            continue;
        }
        let len = [0, 1, 2, 4][usize::from(prefix & 0x3)];
        let data = bytes.get(at + 1..at + 1 + len).ok_or(Refused::Truncated { at })?;
        let unsigned = data.iter().rev().fold(0u32, |v, b| v << 8 | u32::from(*b));
        let signed = match len {
            1 => i32::from(data[0] as i8),
            2 => i32::from(i16::from_le_bytes([data[0], data[1]])),
            4 => unsigned as i32,
            _ => 0,
        };
        match (prefix >> 2 & 0x3, prefix >> 4) {
            // Input (§6.2.2.4).
            (0, 0x8) => {
                if depth == 0 {
                    return Err(Refused::Outside { at });
                }
                let open = collections.len() - 1;
                if globals.size == 0 || globals.size > 32 {
                    flaws[open].get_or_insert(Unbound::ReportSize { bits: globals.size });
                }
                if locals.too_many {
                    flaws[open].get_or_insert(Unbound::TooManyUsages);
                }
                let usages = match (locals.min, locals.max) {
                    (Some(first), Some(last)) if locals.listed.is_empty() => Usages::Range { first, last },
                    _ => Usages::Listed(core::mem::take(&mut locals.listed)),
                };
                let index = match lengths.iter().position(|(id, _)| *id == globals.report) {
                    Some(index) => index,
                    None => {
                        lengths.push((globals.report, 0));
                        lengths.len() - 1
                    }
                };
                let bits = &mut lengths[index].1;
                let field = Field {
                    report: globals.report,
                    bit: *bits,
                    size: globals.size,
                    count: globals.count,
                    data: unsigned & 0x1 == 0,
                    variable: unsigned & 0x2 != 0,
                    relative: unsigned & 0x4 != 0,
                    page: globals.page,
                    usages,
                    logical_min: globals.logical_min,
                    logical_max: globals.logical_max,
                };
                *bits = bits.saturating_add(globals.size.saturating_mul(globals.count));
                let collection = &mut collections[open];
                if globals.report != 0 && !collection.reports.contains(&globals.report) {
                    collection.reports.push(globals.report);
                }
                collection.fields.push(field);
                locals = Locals::default();
            }
            // Output and Feature carry no input bits: only their locals end.
            (0, 0x9) | (0, 0xB) => locals = Locals::default(),
            (0, 0xA) => {
                if depth == NESTING || (depth == 0 && collections.len() == COLLECTIONS) {
                    return Err(Refused::TooDeep { at });
                }
                // A collection at the top opens a device; any inside it is
                // part of that device.
                if depth == 0 {
                    let (page, usage) = match locals.listed.first().or(locals.min.as_ref()) {
                        Some(&u) if u >> 16 != 0 => ((u >> 16) as u16, u as u16),
                        Some(&u) => (globals.page, u as u16),
                        None => (globals.page, 0),
                    };
                    let bound = Err(Unbound::NotAKeyboardOrPointer { page, usage });
                    collections.push(Collection { page, usage, reports: Vec::new(), fields: Vec::new(), bound });
                    flaws.push(None);
                }
                depth += 1;
                locals = Locals::default();
            }
            (0, 0xC) => {
                depth = depth.checked_sub(1).ok_or(Refused::Unbalanced { at })?;
                locals = Locals::default();
            }
            // Global items (§6.2.2.7).
            (1, 0x0) => globals.page = unsigned as u16,
            (1, 0x1) => globals.logical_min = signed,
            // A maximum that reads below a non-negative minimum as signed is
            // unsigned: its top bit is magnitude (§6.2.2.7's remarks).
            (1, 0x2) => {
                globals.logical_max =
                    if signed < globals.logical_min && globals.logical_min >= 0 { unsigned as i32 } else { signed }
            }
            (1, 0x7) => globals.size = unsigned,
            (1, 0x8) => {
                if unsigned == 0 || unsigned > 0xFF {
                    return Err(Refused::ReportId { at });
                }
                globals.report = unsigned as u8;
                numbered = true;
            }
            (1, 0x9) => globals.count = unsigned,
            (1, 0xA) => {
                if stack.len() == GLOBAL_DEPTH {
                    return Err(Refused::GlobalStack { at });
                }
                stack.push(globals);
            }
            (1, 0xB) => globals = stack.pop().ok_or(Refused::GlobalStack { at })?,
            // Local items (§6.2.2.8).
            (2, 0x0) => match locals.listed.len() {
                LISTED => locals.too_many = true,
                _ => locals.listed.push(usage(len, unsigned)),
            },
            (2, 0x1) => locals.min = Some(usage(len, unsigned)),
            (2, 0x2) => locals.max = Some(usage(len, unsigned)),
            // Unit, exponent, physical extent, designators, strings,
            // delimiters and the reserved tags say nothing this reader binds.
            _ => {}
        }
        at += 1 + len;
    }
    if depth != 0 {
        return Err(Refused::Unbalanced { at });
    }
    if numbered && collections.iter().any(|c| c.fields.iter().any(|f| f.report == 0)) {
        return Err(Refused::Unnumbered);
    }
    for (i, collection) in collections.iter().enumerate() {
        let shared = collection.reports.iter().find(|id| collections[i + 1..].iter().any(|c| c.reports.contains(id)));
        if let Some(&id) = shared {
            return Err(Refused::SpansCollections { id });
        }
    }
    for (collection, flaw) in collections.iter_mut().zip(flaws) {
        collection.bound = match flaw {
            Some(flaw) => Err(flaw),
            None => bind(collection, &lengths, numbered),
        };
    }
    Ok(Interface { collections, numbered })
}

/// What `collection` is, from its application usage and the fields it
/// carries, or why it is not bound.
fn bind(collection: &Collection, lengths: &[(u8, u32)], numbered: bool) -> Result<Kind, Unbound> {
    let its = |id: &u8| !numbered || collection.reports.contains(id);
    if let Some(&(id, _)) = lengths
        .iter()
        .find(|(id, bits)| its(id) && (*bits as usize).div_ceil(8) + usize::from(numbered) > MAX_REPORT_BYTES)
    {
        return Err(Unbound::ReportTooLong { id });
    }
    let data = || collection.fields.iter().filter(|f| f.data);
    let axis = |f: &Field| f.names(GENERIC_DESKTOP, X) || f.names(GENERIC_DESKTOP, Y);
    if let Some(f) = data().find(|f| f.logical_max <= f.logical_min && (f.keys() || axis(f))) {
        let (page, usage) = f.control(0).unwrap_or((f.page, 0));
        return Err(Unbound::EmptyRange { page, usage });
    }
    match (collection.page, collection.usage) {
        (GENERIC_DESKTOP, KEYBOARD | KEYPAD) if data().any(Field::keys) => Ok(Kind::Keyboard),
        (GENERIC_DESKTOP, MOUSE | POINTER) if data().any(axis) => Ok(Kind::Pointer),
        (page, usage) => Err(Unbound::NotAKeyboardOrPointer { page, usage }),
    }
}
