//! linux-firmware's container as AMD publishes it, and containers built here
//! to its layout for every refusal and every arm the real one does not reach.

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

/// The AMD bench's Ryzen 7 4800U: family 0x17, model 0x60, stepping 1.
const RENOIR: Signature = Signature(0x0086_0f01);

/// Its firmware's revision, and the one linux-firmware's container holds for
/// it, which Linux loads early on the same machine
/// (`tests/metal/amd-linux/microcode.txt`).
const FIRMWARE: Revision = Revision(0x0860_0106);
const CONTAINER: Revision = Revision(0x0860_010d);

/// Where the Renoir patch's section starts in the container, as AMD's
/// `amd_ucode_info.py` names it (its patch 8 bytes later, 3200 bytes long).
const RENOIR_SECTION: usize = 9764;

fn renoir_patch() -> Patch<'static> {
    let Ok(Choice::Load(patch)) = select(&crate::AMD.0, RENOIR, FIRMWARE) else {
        panic!("the container holds 0x0860010d for the 4800U")
    };
    patch
}

fn put(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

/// A 3200-byte patch: a header naming `id` and nothing else, and a body.
fn patch(id: u32) -> Vec<u8> {
    let mut patch = vec![0; 3200];
    put(&mut patch, 4, id);
    for (i, byte) in patch[PATCH_HEADER..].iter_mut().enumerate() {
        *byte = i as u8;
    }
    patch
}

/// A container: header, a 16-byte equivalence table, then each `(type,
/// bytes)` as a section.
fn container(sections: &[(u32, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    for word in [MAGIC, EQUIVALENCE_TABLE, 16] {
        out.extend(word.to_le_bytes());
    }
    out.extend([0; 16]);
    for (kind, bytes) in sections {
        out.extend(kind.to_le_bytes());
        out.extend((bytes.len() as u32).to_le_bytes());
        out.extend(*bytes);
    }
    out
}

fn refusal(container: &[u8]) -> Refused {
    select(container, RENOIR, FIRMWARE).expect_err("a malformed container selects nothing")
}

#[test]
fn the_4800u_at_its_firmwares_revision_loads_the_containers_patch_in_place() {
    let patch = renoir_patch();
    assert_eq!(patch.revision(), CONTAINER);
    let at = RENOIR_SECTION + SECTION;
    assert!(patch.bytes() == &crate::AMD.0[at..at + 3200], "the bytes are not the section's patch");
}

#[test]
fn an_equal_revision_loads_again_and_a_newer_one_is_current() {
    assert!(matches!(select(&crate::AMD.0, RENOIR, CONTAINER), Ok(Choice::Load(p)) if p.revision() == CONTAINER));
    assert_eq!(select(&crate::AMD.0, RENOIR, Revision(0x0860_010e)), Ok(Choice::Current(CONTAINER)));
}

#[test]
fn another_stepping_is_named_by_no_patch() {
    assert_eq!(select(&crate::AMD.0, Signature(0x0086_0f00), FIRMWARE), Ok(Choice::NoMatch));
}

#[test]
fn a_family_0x17_patch_drops_its_mapping_and_a_later_familys_does_not() {
    assert!(renoir_patch().leaves_its_mapping_uncached());
    let milan = patch(0x0a00_1173);
    let built = container(&[(PATCH, &milan)]);
    let Ok(Choice::Load(patch)) = select(&built, Signature(0x00a0_0f11), Revision(0x0a00_1100)) else {
        panic!("0x0a001173 names CPUID 0x00a00f11")
    };
    assert!(!patch.leaves_its_mapping_uncached());
}

#[test]
fn the_newest_patch_naming_the_cpu_is_chosen_wherever_it_sits() {
    let (older, newer) = (patch(0x0860_0106), patch(0x0860_0110));
    for order in [[&older[..], &newer[..]], [&newer[..], &older[..]]] {
        let built = container(&[(PATCH, order[0]), (PATCH, order[1])]);
        let Ok(Choice::Load(patch)) = select(&built, RENOIR, FIRMWARE) else { panic!("0x08600110 names it") };
        assert_eq!(patch.revision(), Revision(0x0860_0110));
    }
}

#[test]
fn a_container_that_is_not_one_is_refused() {
    let good = container(&[(PATCH, &patch(0x0860_010d))]);
    let mut magic = good.clone();
    put(&mut magic, 0, 0x0041_4d45);
    assert_eq!(refusal(&magic), Refused { at: 0, why: Refusal::Magic(0x0041_4d45) });
    let mut table = good.clone();
    put(&mut table, 4, PATCH);
    assert_eq!(refusal(&table), Refused { at: 0, why: Refusal::TableType(PATCH) });
    assert_eq!(refusal(&good[..HEADER - 1]).why, Refusal::Truncated { need: HEADER, have: HEADER - 1 });
    assert_eq!(refusal(&good[..HEADER + 15]).why, Refusal::Truncated { need: HEADER + 16, have: HEADER + 15 });
}

#[test]
fn a_section_that_is_not_a_patch_is_refused_a_second_containers_magic_among_them() {
    let first = HEADER + 16;
    for kind in [EQUIVALENCE_TABLE, 2, MAGIC] {
        let built = container(&[(PATCH, &patch(0x0860_010d)), (kind, &patch(0x0860_010d))]);
        assert_eq!(refusal(&built), Refused { at: first + SECTION + 3200, why: Refusal::SectionType(kind) });
    }
}

#[test]
fn a_patch_shorter_than_its_header_or_than_its_section_says_is_refused() {
    let short = container(&[(PATCH, &patch(0x0860_010d)[..PATCH_HEADER - 1])]);
    assert_eq!(refusal(&short).why, Refusal::Short(PATCH_HEADER as u32 - 1));
    let whole = container(&[(PATCH, &patch(0x0860_010d))]);
    let cut = &whole[..whole.len() - 1];
    assert_eq!(refusal(cut).why, Refusal::Truncated { need: SECTION + 3200, have: SECTION + 3199 });
    assert_eq!(refusal(&whole[..whole.len() - 3200 - 1]).why, Refusal::Truncated { need: SECTION, have: SECTION - 1 });
}

#[test]
fn a_chipset_specific_patch_is_refused() {
    for (at, north, south) in [(16, 1, 0), (20, 0, 1)] {
        let mut chipset = patch(0x0860_010d);
        put(&mut chipset, at, 1);
        let built = container(&[(PATCH, &chipset)]);
        assert_eq!(refusal(&built).why, Refusal::Chipset { north, south });
    }
}

#[test]
fn a_patch_naming_a_minimum_revision_is_refused() {
    let mut minimum = patch(0x0860_010d);
    put(&mut minimum, 12, 0x0860_0100);
    assert_eq!(refusal(&container(&[(PATCH, &minimum)])).why, Refusal::MinimumRevision(0x0860_0100));
}

#[test]
fn a_malformed_section_after_the_cpus_own_refuses_the_container() {
    let mut chipset = patch(0x0830_107c);
    put(&mut chipset, 16, 1);
    let built = container(&[(PATCH, &patch(0x0860_010d)), (PATCH, &chipset)]);
    let at = HEADER + 16 + SECTION + 3200;
    assert_eq!(refusal(&built), Refused { at, why: Refusal::Chipset { north: 1, south: 0 } });
}

/// Linux v7.2's `amd_shas.c` (`tests/linux/`, outside every shipped package's
/// directory; `NOTICE`): each patch ID's SHA-256, by its rows.
fn linux_digests() -> std::collections::BTreeMap<u32, [u8; 32]> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/linux/amd_shas.c");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let hex = |word: &str| u32::from_str_radix(word.trim_start_matches("0x"), 16).expect("a hex word");
    text.split(" { 0x")
        .skip(1)
        .map(|row| {
            let mut words = row.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty());
            let id = hex(words.next().expect("the row's patch ID"));
            let digest: Vec<u8> = words.take(32).map(|w| hex(w) as u8).collect();
            (id, digest.try_into().unwrap_or_else(|d| panic!("patch {id:#x}'s digest is {d:?}")))
        })
        .collect()
}

/// Every patch the embedded container holds, and so every patch `select` can
/// return out of it, is the patch Linux records: the build-time form of the
/// hash Linux checks at run time below EntrySign's cutoff.
#[test]
fn every_patch_in_the_embedded_container_has_linuxs_digest() {
    let linux = linux_digests();
    let container = &crate::AMD.0;
    let table = u32::from_le_bytes(container[8..HEADER].try_into().unwrap()) as usize;
    let mut rest = &container[HEADER + table..];
    let mut patches = 0;
    while !rest.is_empty() {
        let (patch, after) = section(rest).expect("the embedded container is valid");
        let id = patch.revision().0;
        let want = linux.get(&id).unwrap_or_else(|| panic!("amd_shas.c records no patch {id:#x}"));
        assert_eq!(&toyos_sha2::Sha256::digest(patch.bytes()), want, "patch {id:#x}");
        patches += 1;
        rest = after;
    }
    assert_eq!(patches, 7, "the container's README names seven patches");
}
