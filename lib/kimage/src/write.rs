//! The build's half: a kernel section out of what the build read of the
//! kernel's and the supervisor's ELF.

use alloc::vec::Vec;

use toyos_abi::syscall::{SpawnMap, VmProt, MAX_BUILD_ID};

use crate::{FORMAT, MAGIC, PAGE};

/// One function the kernel names a frame by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Symbol {
    pub addr: u64,
    pub size: u64,
    pub name: alloc::string::String,
}

/// Everything a kernel section carries.
#[derive(Clone, Debug, Default)]
pub struct Input {
    pub machine: u16,
    pub entry: u64,
    pub memory: u64,
    pub relro: (u64, u64),
    pub ranges: Vec<(u64, u64, VmProt)>,
    pub relocations: Vec<(u64, u64)>,
    /// Ascending by address, one per address.
    pub symbols: Vec<Symbol>,
    pub supervisor_entry: u64,
    pub supervisor_base: u64,
    pub supervisor_build_id: Vec<u8>,
    pub supervisor_maps: Vec<SpawnMap>,
    /// The image's bytes from address 0.
    pub image: Vec<u8>,
}

impl Input {
    /// The section, as [`crate::Kernel::parse`] reads it.
    pub fn write(&self) -> Vec<u8> {
        assert!(self.supervisor_build_id.len() <= MAX_BUILD_ID, "a build-id past its bound");
        let mut names: Vec<u8> = Vec::new();
        let mut symbols: Vec<u8> = Vec::new();
        for symbol in &self.symbols {
            symbols.extend(symbol.addr.to_le_bytes());
            symbols.extend(symbol.size.to_le_bytes());
            symbols.extend((names.len() as u32).to_le_bytes());
            symbols.extend((symbol.name.len() as u32).to_le_bytes());
            names.extend(symbol.name.as_bytes());
        }
        let mut out: Vec<u8> = Vec::new();
        out.extend(MAGIC);
        out.extend(FORMAT.to_le_bytes());
        out.extend(self.machine.to_le_bytes());
        out.extend(0u16.to_le_bytes());
        for word in [self.entry, self.memory, self.relro.0, self.relro.1] {
            out.extend(word.to_le_bytes());
        }
        for count in [self.ranges.len(), self.relocations.len(), self.symbols.len(), names.len()] {
            out.extend((count as u32).to_le_bytes());
        }
        let image_at_field = out.len();
        out.extend(0u64.to_le_bytes());
        for &(lo, hi, prot) in &self.ranges {
            out.extend(lo.to_le_bytes());
            out.extend(hi.to_le_bytes());
            out.extend((prot as u64).to_le_bytes());
        }
        for &(offset, addend) in &self.relocations {
            out.extend(offset.to_le_bytes());
            out.extend(addend.to_le_bytes());
        }
        out.extend(symbols);
        out.extend(names);
        out.extend(self.supervisor_entry.to_le_bytes());
        out.extend(self.supervisor_base.to_le_bytes());
        out.extend((self.supervisor_build_id.len() as u64).to_le_bytes());
        let mut id = [0u8; MAX_BUILD_ID];
        id[..self.supervisor_build_id.len()].copy_from_slice(&self.supervisor_build_id);
        out.extend(id);
        out.extend((self.supervisor_maps.len() as u64).to_le_bytes());
        for map in &self.supervisor_maps {
            for word in [map.addr, map.len, map.object, map.offset, map.copied, map.prot] {
                out.extend(word.to_le_bytes());
            }
        }
        let image_at = (out.len() as u64).next_multiple_of(PAGE);
        out[image_at_field..image_at_field + 8].copy_from_slice(&image_at.to_le_bytes());
        out.resize(image_at as usize, 0);
        out.extend(&self.image);
        out
    }
}
