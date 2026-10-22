//! The device set: every PCI function the kernel hands out, as one
//! level-triggered snapshot [`SYS_DEVICE_SET_READ`] answers.
//!
//! **A snapshot, never a history.** A reader diffs two snapshots; nothing is
//! lost between them and nothing is replayed. A function that left is absent;
//! one that returned is listed under a new [`Arrival`].
//!
//! **One [`Header`] and then one [`Pci`] record per function**, in
//! the inventory's 64-byte encoding and in rising arrival order. [`decode`] is
//! the reader's boundary and refuses a snapshot that is anything else.
//!
//! [`SYS_DEVICE_SET_READ`]: crate::syscall::SYS_DEVICE_SET_READ
//! [`Arrival`]: crate::inventory::Arrival

use crate::inventory::{Pci, RawRecord, Record, Undecodable, RECORD_BYTES};

/// The first record's kind byte: one no inventory record has.
const KIND_HEADER: u8 = 0x80;

/// What the snapshot says about itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Header {
    /// Rises with every change to the set; equal generations are equal sets.
    pub generation: u64,
    /// How long until the set may change with no interrupt to say so — a
    /// slot settling after its link came up (PCIe 6.0 §6.6.1), or a slot
    /// command the kernel still waits on — in nanoseconds; `None` where
    /// nothing is pending. A reader reads again by then.
    pub settle_in: Option<u64>,
    /// How many function records follow.
    pub functions: u32,
}

impl Header {
    pub fn encode(&self) -> RawRecord {
        let mut raw = [0u8; RECORD_BYTES];
        raw[0] = KIND_HEADER;
        raw[4..8].copy_from_slice(&self.functions.to_le_bytes());
        raw[8..16].copy_from_slice(&self.generation.to_le_bytes());
        raw[16..24].copy_from_slice(&self.settle_in.map_or(0, |nanos| nanos.max(1)).to_le_bytes());
        RawRecord(raw)
    }

    fn decode(raw: &RawRecord) -> Option<Self> {
        let word = |at: usize| u64::from_le_bytes(raw.0[at..at + 8].try_into().expect("eight bytes"));
        (raw.0[0] == KIND_HEADER).then(|| Self {
            functions: u32::from_le_bytes(raw.0[4..8].try_into().expect("four bytes")),
            generation: word(8),
            settle_in: Some(word(16)).filter(|nanos| *nanos != 0),
        })
    }
}

/// Why a snapshot was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Malformed {
    /// The first record is no header.
    NoHeader,
    /// The header counts `said` functions and `carried` records follow it.
    Count { said: u32, carried: usize },
    /// Record `at` did not decode.
    Undecodable { at: usize, why: Undecodable },
    /// Record `at` is an inventory record of another kind.
    NotAFunction { at: usize },
    /// Record `at` names arrival 0, which no function has.
    NoArrival { at: usize },
    /// Record `at`'s arrival is not above the one before it: listed twice, or
    /// out of order.
    Order { at: usize },
}

impl core::fmt::Display for Malformed {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoHeader => f.write_str("its first record is no header"),
            Self::Count { said, carried } => write!(f, "its header counts {said} functions and {carried} records follow"),
            Self::Undecodable { at, why } => write!(f, "record {at}: {why}"),
            Self::NotAFunction { at } => write!(f, "record {at} is no PCI function"),
            Self::NoArrival { at } => write!(f, "record {at} names arrival 0"),
            Self::Order { at } => write!(f, "record {at}'s arrival is not above the one before it"),
        }
    }
}

/// A snapshot that [`decode`] admitted.
#[derive(Clone, Copy, Debug)]
pub struct Snapshot<'a> {
    pub header: Header,
    records: &'a [RawRecord],
}

impl Snapshot<'_> {
    /// Every function, in rising arrival order.
    pub fn functions(&self) -> impl Iterator<Item = Pci> + '_ {
        self.records.iter().map(|raw| match Record::decode(raw) {
            Ok(Record::Pci(pci)) => pci,
            _ => unreachable!("devset: `decode` admitted only PCI records"),
        })
    }
}

/// `records` as a snapshot, or why not.
pub fn decode(records: &[RawRecord]) -> Result<Snapshot<'_>, Malformed> {
    let (first, rest) = records.split_first().ok_or(Malformed::NoHeader)?;
    let header = Header::decode(first).ok_or(Malformed::NoHeader)?;
    if usize::try_from(header.functions).ok() != Some(rest.len()) {
        return Err(Malformed::Count { said: header.functions, carried: rest.len() });
    }
    let mut last = 0;
    for (index, raw) in rest.iter().enumerate() {
        let at = index + 1;
        let pci = match Record::decode(raw) {
            Ok(Record::Pci(pci)) => pci,
            Ok(_) => return Err(Malformed::NotAFunction { at }),
            Err(why) => return Err(Malformed::Undecodable { at, why }),
        };
        match pci.arrival.0 {
            0 => return Err(Malformed::NoArrival { at }),
            arrival if arrival <= last => return Err(Malformed::Order { at }),
            arrival => last = arrival,
        }
    }
    Ok(Snapshot { header, records: rest })
}

/// One function's record in a snapshot.
pub fn function(pci: Pci) -> RawRecord {
    Record::Pci(pci).encode()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inventory::{Arrival, Driven, Loaded, PciAddr, Role};
    use std::vec::Vec;

    fn pci(arrival: u32) -> Pci {
        Pci {
            arrival: Arrival(arrival),
            at: PciAddr { segment: 0, bus: 1, dev: 0, func: 0 },
            vendor: 0x1234,
            device: 0x11e8,
            class: 0xff,
            subclass: 0,
            prog_if: 0,
            driven: Driven::Free,
        }
    }

    fn snapshot(header: Header, functions: &[Pci]) -> Vec<RawRecord> {
        core::iter::once(header.encode()).chain(functions.iter().map(|f| function(*f))).collect()
    }

    fn header(functions: u32) -> Header {
        Header { generation: 9, settle_in: Some(100_000_000), functions }
    }

    #[test]
    fn a_snapshot_round_trips() {
        let raw = snapshot(header(2), &[pci(3), pci(7)]);
        let read = decode(&raw).expect("admitted");
        assert_eq!(read.header, header(2));
        assert_eq!(read.functions().collect::<Vec<_>>(), [pci(3), pci(7)]);
        let none = Header { generation: 1, settle_in: None, functions: 0 };
        assert_eq!(decode(&snapshot(none, &[])).expect("an empty set").header, none);
        // A settle of zero nanoseconds is still a settle.
        let now = Header { settle_in: Some(0), ..none };
        assert_eq!(decode(&snapshot(now, &[])).expect("admitted").header.settle_in, Some(1));
    }

    /// Every way a snapshot can be wrong is refused by name, and none is read
    /// as a shorter or reordered set.
    #[test]
    fn a_malformed_snapshot_is_refused_at_the_boundary() {
        assert_eq!(decode(&[]).unwrap_err(), Malformed::NoHeader);
        assert_eq!(decode(&[function(pci(1))]).unwrap_err(), Malformed::NoHeader);
        assert_eq!(decode(&snapshot(header(3), &[pci(1)])).unwrap_err(), Malformed::Count { said: 3, carried: 1 });
        assert_eq!(decode(&snapshot(header(0), &[pci(1)])).unwrap_err(), Malformed::Count { said: 0, carried: 1 });
        assert_eq!(decode(&snapshot(header(2), &[pci(1), pci(1)])).unwrap_err(), Malformed::Order { at: 2 });
        assert_eq!(decode(&snapshot(header(2), &[pci(5), pci(4)])).unwrap_err(), Malformed::Order { at: 2 });
        assert_eq!(decode(&snapshot(header(1), &[pci(0)])).unwrap_err(), Malformed::NoArrival { at: 1 });
        let mut raw = snapshot(header(1), &[pci(1)]);
        raw[1] = Record::Loaded(Loaded { role: Role::Log, unique_guid: [0x22; 16] }).encode();
        assert_eq!(decode(&raw).unwrap_err(), Malformed::NotAFunction { at: 1 });
        raw[1] = RawRecord::EMPTY;
        assert_eq!(decode(&raw).unwrap_err(), Malformed::Undecodable { at: 1, why: Undecodable::Kind(0) });
        let mut raw = snapshot(header(1), &[pci(1)]);
        raw[1].0[13] = 7;
        assert_eq!(decode(&raw).unwrap_err(), Malformed::Undecodable { at: 1, why: Undecodable::Variant { at: 13, value: 7 } });
    }

    #[test]
    fn the_header_is_no_inventory_record() {
        assert!(Record::decode(&header(0).encode()).is_err());
    }
}
