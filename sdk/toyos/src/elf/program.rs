//! A program laid out for a spawn: the ranges of the child's memory its
//! `PT_LOAD`s are, as `toyos_abi::syscall::SpawnMap`s, its entry and its
//! build-id. The kernel parses no program; it checks each range as an
//! ordinary mapping and builds the child from them. The build lays the
//! supervisor out with the same function, ahead of the boot.

use toyos_abi::syscall::{SpawnMap, VmProt, MAX_BUILD_ID, MAX_SPAWN_MAPS};
use toyos_abi::HANDLE_INVALID;

use crate::elf::{note, Layout, Machine};

/// The page the ranges are whole ones of.
const PAGE: u64 = 4096;

/// Where a spawned program's lowest page goes in its child: above the clock
/// page (`toyos_abi::clock::CLOCK_PAGE`), in a space nothing else holds yet.
pub const BASE: u64 = 0x2_0000_0000;

/// What a program asks of a spawn: its ranges, its entry, where its address 0
/// lands and its build-id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Program {
    maps: [SpawnMap; MAX_SPAWN_MAPS],
    count: usize,
    pub entry: u64,
    pub image_base: u64,
    build_id: [u8; MAX_BUILD_ID],
    build_id_len: usize,
}

impl Program {
    /// The ranges, each a `PT_LOAD` copied from the object or a hole between
    /// two, reserved, in address order.
    pub fn maps(&self) -> &[SpawnMap] {
        &self.maps[..self.count]
    }

    pub fn build_id(&self) -> &[u8] {
        &self.build_id[..self.build_id_len]
    }
}

/// The program `file`, an image for `machine`, laid out with its lowest page
/// at `at`, each `PT_LOAD` copied from `object`, the memory object holding
/// `file`. Refused by name where the kernel would refuse a range, or the
/// program could not start: no `PT_LOAD` maps its program header table, which
/// it reads to relocate itself, or a segment asks to be written and executed,
/// which no page is, as the loader refuses a library's (`crate::dl`).
pub fn lay_out(file: &[u8], machine: Machine, object: u64, at: u64) -> Result<Program, &'static str> {
    let layout = Layout::parse(file, machine).map_err(|e| e.as_str())?;
    if layout.program_headers().is_none() {
        return Err("ELF: no PT_LOAD maps the program header table");
    }
    if layout.overlapping_load_pages(PAGE).is_some() {
        return Err("ELF: two PT_LOAD segments share a page");
    }
    if !at.is_multiple_of(PAGE) {
        return Err("a program's base is a page boundary");
    }
    let image_base = at.checked_sub(layout.extent().min()).ok_or("ELF: an image below its own base")?;
    let mut program = Program {
        maps: [SpawnMap::default(); MAX_SPAWN_MAPS],
        count: 0,
        entry: at.checked_add(layout.entry().get()).ok_or("ELF: an entry past the address space")?,
        image_base,
        build_id: [0; MAX_BUILD_ID],
        build_id_len: 0,
    };
    let mut segments: [(u64, u64, u64, u64, VmProt); crate::elf::MAX_LOAD_SEGMENTS] = [(0, 0, 0, 0, VmProt::None); crate::elf::MAX_LOAD_SEGMENTS];
    let mut n = 0;
    for seg in layout.segments() {
        let (lo, hi) = seg.page_range(PAGE);
        if hi == lo {
            continue;
        }
        // The file's bytes from the segment's first page: the bytes of that
        // page before the segment are the file's too, as a linker lays them.
        let offset = seg.file_offset() & !(PAGE - 1);
        let bytes = seg.filesz().checked_add(seg.file_offset() % PAGE).ok_or("ELF: a segment past the file")?;
        let copied = bytes.min((file.len() as u64).saturating_sub(offset));
        if seg.flags().executable() && seg.flags().writable() {
            return Err("ELF: a PT_LOAD is both writable and executable");
        }
        let prot = if seg.flags().executable() {
            VmProt::ReadExec
        } else if seg.flags().writable() {
            VmProt::ReadWrite
        } else {
            VmProt::Read
        };
        segments[n] = (lo, hi, offset, copied, prot);
        n += 1;
    }
    let segments = &mut segments[..n];
    segments.sort_unstable_by_key(|&(lo, ..)| lo);
    let mut push = |map: SpawnMap| -> Result<(), &'static str> {
        *program.maps.get_mut(program.count).ok_or("ELF: more ranges than a spawn carries")? = map;
        program.count += 1;
        Ok(())
    };
    let address = |offset: u64| at.checked_add(offset).ok_or("ELF: an image past the address space");
    let mut covered = segments.first().map_or(0, |&(lo, ..)| lo);
    for &(lo, hi, offset, copied, prot) in segments.iter() {
        if lo > covered {
            let hole = SpawnMap { addr: address(covered)?, len: lo - covered, object: u64::from(HANDLE_INVALID.0), ..SpawnMap::default() };
            push(hole)?;
        }
        let len = hi.checked_sub(lo).filter(|_| hi != u64::MAX).ok_or("ELF: a segment past the address space")?;
        push(SpawnMap { addr: address(lo)?, len, object, offset, copied, prot: prot as u64 })?;
        covered = hi;
    }
    if let Some(id) = note::build_id_in(file) {
        program.build_id[..id.len()].copy_from_slice(id);
        program.build_id_len = id.len();
    }
    Ok(program)
}
