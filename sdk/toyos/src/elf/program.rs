//! A program laid out for a spawn: the ranges of the child's memory its
//! `PT_LOAD`s are, as `toyos_abi::syscall::SpawnMap`s, its entry and its
//! build-id. The kernel parses no program; it checks each range as an
//! ordinary mapping and builds the child from them. A segment the program
//! cannot write is the pages of its file where nobody can write that
//! ([`Source::Immutable`]), and a copy of them otherwise. Below the program lies
//! what its parent tells it (`crate::startup`), zeros until a spawn names a
//! block. The build lays the supervisor out with the same function, ahead of
//! the boot.

use toyos_abi::syscall::{SpawnBacking, SpawnMap, VmProt, MAX_BUILD_ID, MAX_SPAWN_MAPS};
use toyos_abi::HANDLE_INVALID;

use crate::elf::{note, Layout, Machine};
use crate::startup::{self, Block};

/// The page the ranges are whole ones of.
const PAGE: u64 = 4096;

/// Where a spawned program's lowest page goes in its child: above the clock
/// page (`toyos_abi::clock::CLOCK_PAGE`) and the startup block, in a space
/// nothing else holds yet.
pub const BASE: u64 = 0x2_0000_0000;

/// The memory object holding a program's file, by the handle a spawn's
/// ranges name it with (`SpawnMap::object`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// One nobody can write.
    Immutable(u64),
    /// One its holder keeps unchanged while the child takes its pages.
    Copied(u64),
}

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
    /// The ranges in address order, none apart from the next: the startup
    /// block's pages and the reserved ones above them, then each `PT_LOAD`
    /// shared with or copied from the object, or a hole between two, reserved.
    pub fn maps(&self) -> &[SpawnMap] {
        &self.maps[..self.count]
    }

    pub fn build_id(&self) -> &[u8] {
        &self.build_id[..self.build_id_len]
    }

    /// The program, its child starting with `block` in place of zeros.
    pub fn starting_with(mut self, block: &Block) -> Program {
        let pages = (block.len() as u64).next_multiple_of(PAGE).max(PAGE);
        self.maps[0] = SpawnMap {
            addr: startup::AT,
            len: pages,
            object: block.object(),
            copied: block.len() as u64,
            prot: VmProt::ReadWrite as u64,
            ..SpawnMap::default()
        };
        self.maps[1] = reserved(startup::AT + pages, BASE);
        self
    }
}

/// `[lo, hi)` reserved: no page of it is mapped.
fn reserved(lo: u64, hi: u64) -> SpawnMap {
    SpawnMap { addr: lo, len: hi - lo, object: u64::from(HANDLE_INVALID.0), ..SpawnMap::default() }
}

/// The program `file`, an image for `machine`, laid out with its lowest page
/// at [`BASE`], each `PT_LOAD` out of `source`, the memory object holding
/// `file`, below it a startup block that says nothing. Refused by name where
/// the kernel would refuse a range, or the program could not start: no
/// `PT_LOAD` maps its program header table, which it reads to relocate itself,
/// or a segment asks to be written and executed, which no page is, as the
/// loader refuses a library's (`crate::dl`).
pub fn lay_out(file: &[u8], machine: Machine, source: Source) -> Result<Program, &'static str> {
    let layout = Layout::parse(file, machine).map_err(|e| e.as_str())?;
    if layout.program_headers().is_none() {
        return Err("ELF: no PT_LOAD maps the program header table");
    }
    if layout.overlapping_load_pages(PAGE).is_some() {
        return Err("ELF: two PT_LOAD segments share a page");
    }
    let image_base = BASE.checked_sub(layout.extent().min()).ok_or("ELF: an image below its own base")?;
    let mut maps = [SpawnMap::default(); MAX_SPAWN_MAPS];
    maps[0] = SpawnMap { addr: startup::AT, len: PAGE, object: u64::from(HANDLE_INVALID.0), prot: VmProt::ReadWrite as u64, ..SpawnMap::default() };
    maps[1] = reserved(startup::AT + PAGE, BASE);
    let mut program = Program {
        maps,
        count: 2,
        entry: BASE.checked_add(layout.entry().get()).ok_or("ELF: an entry past the address space")?,
        image_base,
        build_id: [0; MAX_BUILD_ID],
        build_id_len: 0,
    };
    let (object, immutable) = match source {
        Source::Immutable(object) => (object, true),
        Source::Copied(object) => (object, false),
    };
    let mut segments = [(0, 0, 0, 0, VmProt::None, SpawnBacking::Copy); crate::elf::MAX_LOAD_SEGMENTS];
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
        // A shared range copies nothing.
        let (backing, copied) = match immutable && seg.is_file_pages(PAGE, file.len() as u64) {
            true => (SpawnBacking::Share, 0),
            false => (SpawnBacking::Copy, bytes.min((file.len() as u64).saturating_sub(offset))),
        };
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
        segments[n] = (lo, hi, offset, copied, prot, backing);
        n += 1;
    }
    let segments = &mut segments[..n];
    segments.sort_unstable_by_key(|&(lo, ..)| lo);
    let mut push = |map: SpawnMap| -> Result<(), &'static str> {
        *program.maps.get_mut(program.count).ok_or("ELF: more ranges than a spawn carries")? = map;
        program.count += 1;
        Ok(())
    };
    let address = |offset: u64| BASE.checked_add(offset).ok_or("ELF: an image past the address space");
    let mut covered = segments.first().map_or(0, |&(lo, ..)| lo);
    for &(lo, hi, offset, copied, prot, backing) in segments.iter() {
        if lo > covered {
            push(reserved(address(covered)?, address(lo)?))?;
        }
        let len = hi.checked_sub(lo).filter(|_| hi != u64::MAX).ok_or("ELF: a segment past the address space")?;
        push(SpawnMap { addr: address(lo)?, len, object, offset, copied, prot: prot as u64, backing: backing as u64 })?;
        covered = hi;
    }
    if let Some(id) = note::build_id_in(file) {
        program.build_id[..id.len()].copy_from_slice(id);
        program.build_id_len = id.len();
    }
    Ok(program)
}
