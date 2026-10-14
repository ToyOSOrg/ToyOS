//! AMD's Family 17h container as AMD publishes it, containers built here in
//! the same layout for every refusal and arm it does not reach, and the
//! choice among patches.

#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a test fails by panicking"
)]

extern crate std;

use std::vec;
use std::vec::Vec;

use super::*;
use crate::{loader, may_load, Loader, Unloaded, AMD};
use toyos_cpuvuln::Vendor;

/// `amd-ucode/microcode_amd_fam17h.bin`, pinned by its digest in `NOTICE`.
const FILE: &[u8] = include_bytes!("../../amd-ucode/microcode_amd_fam17h.bin");

/// The AMD bench's Ryzen 7 4800U at the level its firmware loads.
const BENCH: Cpu = Cpu { signature: Signature(0x0086_0f01), level: Revision(0x0860_0106) };

/// Where the bench's patch section starts in [`FILE`], and its payload's length.
const BENCH_SECTION: usize = 0x2624;
const LEN: usize = 3200;

/// Every table entry of [`FILE`] and the level of the patch its equivalence
/// ID names, in file order.
const ENTRIES: [(u32, u16, u32); 7] = [
    (0x0087_0f10, 0x8710, 0x0870_1034),
    (0x0080_0f12, 0x8012, 0x0800_126f),
    (0x0083_0f10, 0x8310, 0x0830_107c),
    (0x0086_0f01, 0x8601, 0x0860_010d),
    (0x0080_0f82, 0x8082, 0x0800_820d),
    (0x008a_0f00, 0x8a00, 0x08a0_000a),
    (0x0086_0f81, 0x8681, 0x0860_8108),
];

fn put(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

/// A 64-byte MPB: its header naming `id` at `level`, every other header
/// field 0, then 32 bytes of data.
fn mpb(id: u16, level: u32) -> Vec<u8> {
    let mut block = vec![0; 64];
    put(&mut block, 4, level);
    put(&mut block, 0x18, id.into());
    for (i, byte) in block[MPB_HEADER..].iter_mut().enumerate() {
        *byte = (i as u8).wrapping_mul(0x9d) ^ level as u8;
    }
    block
}

/// A container in the layout AMD's files have: the table of `entries` and
/// its all-zero entry, then each block as a patch section.
fn container(entries: &[(u32, u16)], blocks: &[Vec<u8>]) -> Vec<u8> {
    let mut file = Vec::new();
    for word in [MAGIC, TABLE, (ENTRY * (entries.len() + 1)) as u32] {
        file.extend_from_slice(&word.to_le_bytes());
    }
    for &(signature, id) in entries {
        for word in [signature, 0, 0, id.into()] {
            file.extend_from_slice(&word.to_le_bytes());
        }
    }
    file.extend_from_slice(&[0; ENTRY]);
    for block in blocks {
        file.extend_from_slice(&PATCH.to_le_bytes());
        file.extend_from_slice(&(block.len() as u32).to_le_bytes());
        file.extend_from_slice(block);
    }
    file
}

/// The bench's own entry and one patch for it at `level`.
fn bench_container(level: u32) -> Vec<u8> {
    container(&[(BENCH.signature.0, 0x8601)], &[mpb(0x8601, level)])
}

fn refusal(file: &[u8]) -> Refused {
    select(file, &BENCH).expect_err("a malformed file selects nothing")
}

fn loads<'a>(file: &'a [u8], cpu: &Cpu) -> Patch<'a> {
    match select(file, cpu) {
        Ok(Choice::Load(patch)) => patch,
        other => panic!("{cpu:x?} loads nothing: {other:x?}"),
    }
}

#[test]
fn the_bench_is_handed_amds_patch_in_place_in_the_container() {
    let patch = loads(FILE, &BENCH);
    assert_eq!(patch.level(), Revision(0x0860_010d));
    let payload = BENCH_SECTION + SECTION;
    assert!(patch.block() == &FILE[payload..payload + LEN], "the block is not the bench's section payload");
}

#[test]
fn every_cpu_the_table_names_is_handed_the_patch_its_equivalence_id_names() {
    for (i, (signature, _, level)) in ENTRIES.into_iter().enumerate() {
        let cpu = Cpu { signature: Signature(signature), level: Revision(0) };
        let patch = loads(FILE, &cpu);
        assert_eq!(patch.level(), Revision(level), "{signature:#x}");
        let payload = CONTAINER + 8 * ENTRY + i * (SECTION + LEN) + SECTION;
        assert!(patch.block() == &FILE[payload..payload + LEN], "{signature:#x}'s block is not its section's");
    }
}

#[test]
fn a_cpu_at_the_patchs_level_is_handed_it_and_one_above_is_not() {
    let at = Cpu { level: Revision(0x0860_010d), ..BENCH };
    assert_eq!(loads(FILE, &at).level(), Revision(0x0860_010d));
    let above = Cpu { level: Revision(0x0860_010e), ..BENCH };
    assert_eq!(select(FILE, &above), Ok(Choice::Current(Revision(0x0860_010d))));
}

#[test]
fn a_signature_the_table_does_not_name_matches_nothing() {
    for signature in [0x0086_0f00, 0x0086_0f02, 0x0086_1f01, 0x0000_8601, 0] {
        let cpu = Cpu { signature: Signature(signature), ..BENCH };
        assert_eq!(select(FILE, &cpu), Ok(Choice::NoMatch), "{signature:#x}");
    }
}

/// The table, not the pattern AMD's levels and IDs follow, decides: an entry
/// mapping 0x00860f01 to another ID matches no patch, though the bench's
/// patch's level and ID both decode to 0x00860f01.
#[test]
fn the_table_alone_maps_a_signature_to_its_patch() {
    let mut file = FILE.to_vec();
    let entry = CONTAINER + 3 * ENTRY;
    assert_eq!(&file[entry..entry + 4], &0x0086_0f01u32.to_le_bytes());
    put(&mut file, entry + 12, 0x8602);
    assert_eq!(select(&file, &BENCH), Ok(Choice::NoMatch));
    // And a patch whose ID the table names for the bench is the bench's,
    // whatever its level decodes to.
    let other = container(&[(BENCH.signature.0, 0x8681)], &[mpb(0x8601, 0x0860_010d), mpb(0x8681, 0x0860_8108)]);
    assert_eq!(loads(&other, &BENCH).level(), Revision(0x0860_8108));
}

#[test]
fn the_newest_patch_for_the_cpu_is_chosen_wherever_it_sits() {
    let older = mpb(0x8601, 0x0860_0105);
    let newest = mpb(0x8601, 0x0860_010d);
    let other = mpb(0x8681, 0x0860_9000);
    for blocks in [[&older, &newest, &other], [&other, &newest, &older], [&newest, &older, &other]] {
        let file = container(&[(BENCH.signature.0, 0x8601), (0x0086_0f81, 0x8681)], &blocks.map(|b| b.clone()));
        let patch = loads(&file, &BENCH);
        assert_eq!(patch.level(), Revision(0x0860_010d));
        assert!(patch.block() == &newest[..], "the block is not the newest patch's");
    }
}

#[test]
fn a_cpu_above_the_newest_patch_is_handed_no_older_one() {
    let file = container(
        &[(BENCH.signature.0, 0x8601)],
        &[mpb(0x8601, 0x0860_0105), mpb(0x8601, 0x0860_010d), mpb(0x8601, 0x0860_0101)],
    );
    for level in [0x0860_010e, u32::MAX] {
        let cpu = Cpu { level: Revision(level), ..BENCH };
        assert_eq!(select(&file, &cpu), Ok(Choice::Current(Revision(0x0860_010d))));
    }
}

#[test]
fn two_patches_at_the_cpus_newest_level_are_refused() {
    let mut second = mpb(0x8601, 0x0860_010d);
    second[40] ^= 1;
    let file = container(&[(BENCH.signature.0, 0x8601)], &[mpb(0x8601, 0x0860_010d), second]);
    let at = CONTAINER + 2 * ENTRY + SECTION + 64;
    assert_eq!(refusal(&file), Refused { at, why: Refusal::TwoNewest(Revision(0x0860_010d)) });
    // Two at a level below the newest choose nothing between them.
    let below = container(
        &[(BENCH.signature.0, 0x8601)],
        &[mpb(0x8601, 0x0860_0105), mpb(0x8601, 0x0860_0105), mpb(0x8601, 0x0860_010d)],
    );
    assert_eq!(loads(&below, &BENCH).level(), Revision(0x0860_010d));
    // Nor do two for another CPU.
    let other = container(&[(BENCH.signature.0, 0x8601)], &[mpb(0x8681, 1), mpb(0x8681, 1), mpb(0x8601, 0x0860_010d)]);
    assert_eq!(loads(&other, &BENCH).level(), Revision(0x0860_010d));
}

#[test]
fn a_patch_whose_id_no_entry_names_matches_nothing() {
    let file = container(&[(BENCH.signature.0, 0x8601)], &[mpb(0x8602, 0x0860_010d)]);
    assert_eq!(select(&file, &BENCH), Ok(Choice::NoMatch));
    let none = container(&[(BENCH.signature.0, 0x8601)], &[]);
    assert_eq!(select(&none, &BENCH), Ok(Choice::NoMatch));
}

#[test]
fn a_file_cut_anywhere_but_a_section_boundary_is_refused() {
    let boundary = |len: usize| len >= CONTAINER + 8 * ENTRY && (len - CONTAINER - 8 * ENTRY) % (SECTION + LEN) == 0;
    for len in (0..FILE.len()).filter(|&len| !boundary(len)) {
        assert!(matches!(refusal(&FILE[..len]).why, Refusal::Truncated { .. }), "a cut at {len} was not refused");
    }
    assert_eq!(refusal(&FILE[..CONTAINER - 1]), Refused { at: 0, why: Refusal::Truncated { need: CONTAINER, have: 11 } });
    assert_eq!(
        refusal(&FILE[..CONTAINER + 8 * ENTRY - 1]),
        Refused { at: 0, why: Refusal::Truncated { need: CONTAINER + 8 * ENTRY, have: CONTAINER + 8 * ENTRY - 1 } },
    );
    let why = Refusal::Truncated { need: SECTION + LEN, have: SECTION + LEN - 1 };
    assert_eq!(refusal(&FILE[..FILE.len() - 1]), Refused { at: FILE.len() - SECTION - LEN, why });
    // Cut where the bench's section starts, the file is whole and holds
    // nothing for it.
    assert_eq!(select(&FILE[..BENCH_SECTION], &BENCH), Ok(Choice::NoMatch));
}

#[test]
fn bytes_after_the_last_section_are_refused() {
    for extra in 1..SECTION {
        let mut file = FILE.to_vec();
        file.resize(FILE.len() + extra, 0);
        let why = Refusal::Truncated { need: SECTION, have: extra };
        assert_eq!(refusal(&file), Refused { at: FILE.len(), why });
    }
}

#[test]
fn a_container_with_another_magic_or_first_section_is_refused() {
    let mut file = FILE.to_vec();
    file[3] = 1;
    assert_eq!(refusal(&file), Refused { at: 0, why: Refusal::Magic(0x0141_4d44) });
    let mut file = FILE.to_vec();
    put(&mut file, 4, PATCH);
    assert_eq!(refusal(&file), Refused { at: 0, why: Refusal::TableType(PATCH) });
}

#[test]
fn a_table_of_no_entries_or_part_of_one_is_refused() {
    for len in [0, 8, 15, 17, 120] {
        let mut file = bench_container(1);
        put(&mut file, 8, len);
        assert_eq!(refusal(&file), Refused { at: 0, why: Refusal::TableSize(len as usize) }, "length {len}");
    }
    let mut file = bench_container(1);
    put(&mut file, 8, u32::MAX & !15);
    assert!(matches!(refusal(&file).why, Refusal::Truncated { .. }));
}

#[test]
fn a_table_without_its_all_zero_entry_is_refused() {
    for byte in [0, 4, 8, 12, 14, 15] {
        let mut file = FILE.to_vec();
        file[CONTAINER + 7 * ENTRY + byte] = 1;
        assert_eq!(refusal(&file), Refused { at: CONTAINER + 7 * ENTRY, why: Refusal::Unterminated }, "byte {byte}");
    }
}

#[test]
fn an_entry_with_signature_or_id_0_before_the_last_is_refused() {
    for field in [0, 12] {
        let mut file = FILE.to_vec();
        put(&mut file, CONTAINER + 2 * ENTRY + field, 0);
        assert_eq!(refusal(&file), Refused { at: CONTAINER + 2 * ENTRY, why: Refusal::EmptyEntry }, "field {field}");
    }
    // The reserved half beside the ID is not part of it.
    let mut file = FILE.to_vec();
    file[CONTAINER + 2 * ENTRY + 14] = 0xff;
    assert_eq!(loads(&file, &BENCH).level(), Revision(0x0860_010d));
}

#[test]
fn an_entry_with_errata_fields_is_refused() {
    for field in [4, 8] {
        let mut file = FILE.to_vec();
        put(&mut file, CONTAINER + 5 * ENTRY + field, 1 << 31);
        assert_eq!(refusal(&file), Refused { at: CONTAINER + 5 * ENTRY, why: Refusal::Errata }, "field {field}");
    }
}

#[test]
fn a_signature_mapped_twice_is_refused() {
    for id in [0x8601, 0x8602] {
        let file = container(&[(BENCH.signature.0, 0x8601), (0x0086_0f81, 0x8681), (BENCH.signature.0, id)], &[]);
        let why = Refusal::DuplicateSignature(BENCH.signature.0);
        assert_eq!(refusal(&file), Refused { at: CONTAINER + 2 * ENTRY, why }, "{id:#x}");
    }
}

#[test]
fn a_section_that_is_not_a_patch_is_refused() {
    let mut file = FILE.to_vec();
    put(&mut file, BENCH_SECTION, 2);
    assert_eq!(refusal(&file), Refused { at: BENCH_SECTION, why: Refusal::SectionType(2) });
    let mut file = FILE.to_vec();
    file.extend_from_slice(&bench_container(0x0860_0200));
    assert_eq!(refusal(&file), Refused { at: FILE.len(), why: Refusal::SecondContainer });
}

#[test]
fn a_patch_shorter_than_its_header_is_refused() {
    for len in [0, 31] {
        let mut file = container(&[(BENCH.signature.0, 0x8601)], &[mpb(0x8601, 1)[..len].to_vec()]);
        let at = CONTAINER + 2 * ENTRY;
        assert_eq!(refusal(&file), Refused { at, why: Refusal::PatchSize(len) });
        file.truncate(at + SECTION);
        put(&mut file, at + 4, 32);
        file.extend_from_slice(&[0; 32]);
        assert_eq!(select(&file, &BENCH), Ok(Choice::NoMatch), "a 32-byte block naming ID 0");
    }
}

#[test]
fn a_header_this_loader_does_not_understand_is_refused() {
    let at = CONTAINER + 2 * ENTRY;
    for (offset, value, why) in [
        (0x10, 1, Refusal::ChipsetSpecific),
        (0x14, 0x8000_0000, Refusal::ChipsetSpecific),
        (0x18, 0x0001_8601, Refusal::ProcessorIdHigh(1)),
        (0x18, 0x8000_8601, Refusal::ProcessorIdHigh(0x8000)),
        (0x1c, 0x0001_0000, Refusal::Chained),
        (0x1c, 0x8000_0000, Refusal::Chained),
    ] {
        let mut file = bench_container(0x0860_010d);
        put(&mut file, at + SECTION + offset, value);
        assert_eq!(refusal(&file), Refused { at, why }, "{value:#x} at {offset:#x}");
    }
    // The fields neither the match nor the load reads: the date, the loader
    // ID and what follows it, the BIOS revision and Load Control.
    for (offset, value) in [(0, u32::MAX), (8, u32::MAX), (0xc, u32::MAX), (0x1c, 0xffff)] {
        let mut file = bench_container(0x0860_010d);
        put(&mut file, at + SECTION + offset, value);
        assert_eq!(loads(&file, &BENCH).level(), Revision(0x0860_010d), "{value:#x} at {offset:#x}");
    }
}

#[test]
fn the_embedded_file_is_amds_and_the_benchs_patch_sits_as_it_loaded_on_the_bench() {
    assert!(AMD.0 == *FILE, "the embedded file is not amd-ucode/microcode_amd_fam17h.bin");
    let patch = loads(&AMD.0, &BENCH);
    // The bench loaded it in place at an address 0xC past a 16-byte boundary.
    assert_eq!(patch.block().as_ptr() as usize % 16, 0xc);
}

#[test]
fn the_patch_level_is_msr_8bhs_low_dword() {
    assert_eq!(Revision::from_patch_level(0xffff_ffff_0860_010d), Revision(0x0860_010d));
}

#[test]
fn an_amd_cpu_has_a_loader_and_loads_unless_a_guest() {
    assert_eq!(loader(Vendor::Amd), Ok(Loader::Amd));
    for found in [Revision(0x0860_0106), Revision(u32::MAX)] {
        assert_eq!(may_load(Loader::Amd, 0, found), Ok(()));
        assert_eq!(may_load(Loader::Amd, 1 << 31, found), Err(Unloaded::Hypervisor));
    }
}
