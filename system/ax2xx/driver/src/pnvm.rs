//! The AX210's platform-NVM file (§7.4): records as a firmware file's, with
//! no header, in blocks each a SKU record starts.

use alloc::vec::Vec;

use crate::bytes::{u16_at, u32_at, words};
use crate::container::records;
use crate::part::Hardware;

pub mod kind {
    pub const PAYLOAD: u32 = 19;
    pub const HARDWARE: u32 = 58;
    pub const VERSION: u32 = 62;
    pub const SKU: u32 = 64;
}

/// A payload word that marks a deprecated separator, skipped.
pub const SEPARATOR: u32 = 0xdddd_eeee;

/// One block: the SKU and hardware it is for, and its payloads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block<'a> {
    pub sku: [u32; 3],
    pub hardware: Hardware,
    pub version: u32,
    pub payloads: Vec<&'a [u8]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pnvm<'a> {
    pub blocks: Vec<Block<'a>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    Truncated { at: usize },
    /// A record before the first SKU record.
    Outside { at: usize },
    Unknown { kind: u32, at: usize },
    Length { kind: u32, at: usize, len: usize },
    Repeated { kind: u32, at: usize },
    /// A block without this record.
    Missing { kind: u32, block: usize },
    Empty,
}

/// What the alive notification's SKU identity chooses (§7.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice<'p, 'a> {
    /// An identity of all zeros: the platform-NVM step is skipped.
    Skip,
    Block(&'p Block<'a>),
    /// No block is for this SKU and hardware.
    None,
}

/// A block as its records arrive, each singleton read once.
struct Open<'a> {
    sku: [u32; 3],
    hardware: Option<Hardware>,
    version: Option<u32>,
    payloads: Vec<&'a [u8]>,
}

impl<'a> Open<'a> {
    fn close(self, block: usize) -> Result<Block<'a>, Refused> {
        let missing = |kind| Refused::Missing { kind, block };
        Ok(Block {
            sku: self.sku,
            hardware: self.hardware.ok_or(missing(kind::HARDWARE))?,
            version: self.version.ok_or(missing(kind::VERSION))?,
            payloads: self.payloads,
        })
    }
}

impl<'a> Pnvm<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Pnvm<'a>, Refused> {
        let mut blocks = Vec::new();
        let mut open: Option<Open<'a>> = None;
        for record in records(bytes, 0) {
            let record = record.map_err(|at| Refused::Truncated { at })?;
            let (kind, at, contents) = (record.kind, record.at, record.contents);
            let length = Refused::Length { kind, at, len: contents.len() };
            if kind == kind::SKU {
                let sku = match (contents.len(), words(contents)) {
                    (12, Some(mut w)) => [w.next(), w.next(), w.next()],
                    _ => return Err(length),
                };
                let [Some(a), Some(b), Some(c)] = sku else { return Err(length) };
                if let Some(done) = open.replace(Open { sku: [a, b, c], hardware: None, version: None, payloads: Vec::new() }) {
                    blocks.push(done.close(blocks.len())?);
                }
                continue;
            }
            let block = open.as_mut().ok_or(Refused::Outside { at })?;
            let repeated = Refused::Repeated { kind, at };
            match kind {
                kind::HARDWARE => {
                    let hardware = match (u16_at(contents, 0), u16_at(contents, 2)) {
                        (Some(mac), Some(rf)) => Hardware { mac, rf },
                        _ => return Err(length),
                    };
                    if block.hardware.replace(hardware).is_some() {
                        return Err(repeated);
                    }
                }
                kind::VERSION => {
                    let version = match contents.len() {
                        4 => u32_at(contents, 0).ok_or(length)?,
                        _ => return Err(length),
                    };
                    if block.version.replace(version).is_some() {
                        return Err(repeated);
                    }
                }
                kind::PAYLOAD => match (u32_at(contents, 0), contents.get(4..)) {
                    (Some(SEPARATOR), _) => {}
                    (Some(_), Some(payload)) => block.payloads.push(payload),
                    _ => return Err(length),
                },
                kind => return Err(Refused::Unknown { kind, at }),
            }
        }
        let last = open.ok_or(Refused::Empty)?;
        blocks.push(last.close(blocks.len())?);
        Ok(Pnvm { blocks })
    }

    /// The block for the SKU identity the alive notification reported and the
    /// hardware the part reads as.
    pub fn choose(&self, sku: [u32; 3], hardware: Hardware) -> Choice<'_, 'a> {
        if sku == [0; 3] {
            return Choice::Skip;
        }
        match self.blocks.iter().find(|b| b.sku == sku && b.hardware == hardware) {
            Some(block) => Choice::Block(block),
            None => Choice::None,
        }
    }
}
