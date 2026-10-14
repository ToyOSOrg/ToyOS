//! Intel's files as Intel publishes them, and updates built here to the SDM's
//! layout for every refusal and every arm the real files do not reach.

#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a test fails by panicking"
)]

use toyos_microcode::{Cpu, PlatformId};

use super::*;

/// `intel-ucode/06-8c-01`, pinned by its digest in `NOTICE`.
const T14_FILE: &[u8] = include_bytes!("../intel-ucode/06-8c-01");

/// `intel-ucode/06-cc-02`, pinned by its digest in `NOTICE`: one update,
/// revision 0x11c, for 0xc06c1 with flags 0x94, and four extended
/// signatures with those flags, the first 0xc06c1 again and the last
/// 0xe0652.
const CC02_FILE: &[u8] = include_bytes!("../intel-ucode/06-cc-02");

/// The T14's i5-1135G7: platform 7 as all eight of its CPUs read
/// `IA32_PLATFORM_ID`.
const T14_SIGNATURE: u32 = 0x0008_06c1;
const T14_PLATFORM: PlatformId = PlatformId::from_msr(0x001c_0000_0000_0000);

fn get(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn put(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

/// Rewrite the header's checksum so the header and data sum to 0 again.
fn reseal(update: &mut [u8], data: usize) {
    put(update, 16, 0);
    let sum = sum(&update[..HEADER + data]);
    put(update, 16, sum.wrapping_neg());
}

/// A 2048-byte update to Tables 12-7 to 12-10: `signature` and `flags` in the
/// header, `extended` as its extended signature table, every checksum true.
fn build(signature: u32, flags: u32, revision: u32, extended: &[(u32, u32)]) -> Vec<u8> {
    let table = if extended.is_empty() { 0 } else { EXT_HEADER + EXT_SIGNATURE * extended.len() };
    let data = 2048 - HEADER - table;
    let mut update = vec![0; 2048];
    for (i, word) in update[HEADER..HEADER + data].as_chunks_mut::<4>().0.iter_mut().enumerate() {
        *word = (i as u32).wrapping_mul(0x9e37_79b9).to_le_bytes();
    }
    for (at, value) in [(0, 1), (4, revision), (12, signature), (20, 1), (24, flags)] {
        put(&mut update, at, value);
    }
    put(&mut update, 28, data as u32);
    put(&mut update, 32, 2048);
    reseal(&mut update, data);
    if table != 0 {
        let header = get(&update, 12).wrapping_add(get(&update, 16)).wrapping_add(get(&update, 24));
        let at = HEADER + data;
        put(&mut update, at, extended.len() as u32);
        for (i, &(sig, flags)) in extended.iter().enumerate() {
            let entry = at + EXT_HEADER + EXT_SIGNATURE * i;
            put(&mut update, entry, sig);
            put(&mut update, entry + 4, flags);
            put(&mut update, entry + 8, header.wrapping_sub(sig).wrapping_sub(flags));
        }
        let sum = sum(&update[at..]);
        put(&mut update, at + 4, sum.wrapping_neg());
    }
    update
}

fn refusal(file: &[u8]) -> Refused {
    updates(file).expect_err("a malformed file is refused")
}

/// Every CPU `file`'s updates name, as `(signature, platform flag, revision)`.
fn named(file: &[u8]) -> Vec<(u32, u32, u32)> {
    let updates = updates(file).unwrap_or_else(|refused| panic!("{refused:?}"));
    updates
        .iter()
        .flat_map(|update| update.patches())
        .map(|(cpu, patch)| match cpu {
            Cpu::Intel { signature, platform } => (signature, platform.flag(), patch.revision),
            Cpu::Amd { .. } => panic!("an Intel update names an AMD CPU"),
        })
        .collect()
}

#[test]
fn the_t14s_file_is_one_update_for_the_t14_alone_and_whole() {
    let [update] = updates(T14_FILE).unwrap()[..] else { panic!("06-8c-01 holds one update") };
    let patches: Vec<_> = update.patches().collect();
    let cpu = Cpu::Intel { signature: T14_SIGNATURE, platform: T14_PLATFORM };
    assert_eq!(patches, [(cpu, Patch { revision: 0xbe, bytes: T14_FILE })]);
}

#[test]
fn an_extended_signature_in_intels_file_names_its_cpus() {
    let named = named(CC02_FILE);
    // The header's signature and the four extended ones, each on the three
    // platforms of flags 0x94.
    assert_eq!(named.len(), 15, "{named:x?}");
    for flag in [0x04, 0x10, 0x80] {
        assert!(named.contains(&(0x000e_0652, flag, 0x11c)), "0xe0652 on platform flag {flag:#x}");
    }
    assert!(!named.iter().any(|&(_, flag, _)| flag & 0x94 == 0));
    let [update] = updates(CC02_FILE).unwrap()[..] else { panic!("06-cc-02 holds one update") };
    assert!(update.patches().all(|(_, patch)| patch.bytes == CC02_FILE), "a patch is not the whole update");
}

#[test]
fn a_flipped_bit_anywhere_in_header_or_data_is_refused() {
    let mut file = T14_FILE.to_vec();
    for at in (0..HEADER).chain((HEADER..file.len()).step_by(4093)) {
        file[at] ^= 1;
        assert!(updates(&file).is_err(), "a flip at byte {at} was accepted");
        file[at] ^= 1;
    }
    file[4096] ^= 1;
    assert_eq!(refusal(&file), Refused { at: 0, why: Refusal::Checksum(u32::MAX) });
}

#[test]
fn a_file_cut_short_is_refused() {
    for file in [T14_FILE, CC02_FILE] {
        let len = file.len();
        assert_eq!(refusal(&file[..len - 4]).why, Refusal::Truncated { need: len, have: len - 4 });
    }
    assert_eq!(refusal(&T14_FILE[..HEADER - 1]).why, Refusal::Truncated { need: HEADER, have: HEADER - 1 });
}

#[test]
fn an_empty_file_is_refused() {
    assert_eq!(refusal(&[]), Refused { at: 0, why: Refusal::Truncated { need: HEADER, have: 0 } });
}

#[test]
fn a_malformed_update_after_a_good_one_refuses_the_file() {
    let mut file = T14_FILE.to_vec();
    file.extend_from_slice(&[0; HEADER]);
    assert_eq!(refusal(&file), Refused { at: T14_FILE.len(), why: Refusal::HeaderVersion(0) });
    for have in 1..HEADER {
        let why = Refusal::Truncated { need: HEADER, have };
        assert_eq!(refusal(&file[..T14_FILE.len() + have]), Refused { at: T14_FILE.len(), why });
    }
}

#[test]
fn a_header_or_loader_version_other_than_1_is_refused() {
    for (at, why) in [(0, Refusal::HeaderVersion(2)), (20, Refusal::LoaderRevision(2))] {
        let mut update = build(T14_SIGNATURE, 0x80, 0xc0, &[]);
        put(&mut update, at, 2);
        reseal(&mut update, 2000);
        assert_eq!(refusal(&update).why, why);
    }
}

#[test]
fn a_size_zero_off_its_granule_or_inside_out_is_refused() {
    let cases: [(u32, u32, Refusal); 5] = [
        (0, 0, Refusal::ZeroDataSize),
        (1998, 2048, Refusal::DataSize(1998)),
        (2000, 2047, Refusal::TotalSize(2047)),
        (2000, 2560, Refusal::TotalSize(2560)),
        (3024, 2048, Refusal::TotalBelowData { total: 2048, data: 3024 }),
    ];
    for (data, total, why) in cases {
        let mut update = build(T14_SIGNATURE, 0x80, 0xc0, &[]);
        put(&mut update, 28, data);
        put(&mut update, 32, total);
        assert_eq!(refusal(&update).why, why);
    }
}

#[test]
fn any_multiple_of_1024_is_a_total_size() {
    let mut update = build(T14_SIGNATURE, 0x80, 0xc0, &[]);
    update.resize(3072, 0);
    put(&mut update, 28, 3072 - HEADER as u32);
    put(&mut update, 32, 3072);
    reseal(&mut update, 3072 - HEADER);
    assert_eq!(named(&update), [(T14_SIGNATURE, 0x80, 0xc0)]);
}

#[test]
fn an_extended_signature_names_a_cpu_the_header_does_not() {
    let other = (0x0008_06c2, 0x01);
    let t14 = (T14_SIGNATURE, 0x80);
    for extended in [[other, t14], [t14, other]] {
        let update = build(0x0009_06a3, 0x80, 0xc0, &extended);
        let named = named(&update);
        assert!(named.contains(&(T14_SIGNATURE, 0x80, 0xc0)), "{named:x?}");
        assert!(named.contains(&(0x0008_06c2, 0x01, 0xc0)), "{named:x?}");
    }
    let wrong_platform = build(0x0009_06a3, 0x80, 0xc0, &[(T14_SIGNATURE, 0x01)]);
    assert!(!named(&wrong_platform).contains(&(T14_SIGNATURE, 0x80, 0xc0)));
}

#[test]
fn a_damaged_extended_table_is_refused() {
    let at = 2048 - EXT_HEADER - 2 * EXT_SIGNATURE;
    let good = build(0x0009_06a3, 0x80, 0xc0, &[(0x0008_06c2, 0x01), (T14_SIGNATURE, 0x80)]);

    let mut entry = good.clone();
    let signature = at + EXT_HEADER + EXT_SIGNATURE;
    let (sig, sum) = (get(&entry, signature), get(&entry, at + 4));
    put(&mut entry, signature, sig + 1);
    put(&mut entry, at + 4, sum.wrapping_sub(1));
    assert_eq!(refusal(&entry).why, Refusal::ExtendedSignatureChecksum { index: 1 });

    let mut table = good.clone();
    table[at + 8] ^= 1;
    assert_eq!(refusal(&table).why, Refusal::ExtendedTableChecksum(1));

    for count in [1, 3] {
        let mut wrong = good.clone();
        put(&mut wrong, at, count);
        let len = EXT_HEADER + 2 * EXT_SIGNATURE;
        assert_eq!(refusal(&wrong).why, Refusal::ExtendedTableSize { len }, "count {count}");
    }

    let mut short = build(T14_SIGNATURE, 0x80, 0xc0, &[]);
    put(&mut short, 28, 1984);
    short[2032..].fill(0);
    reseal(&mut short, 1984);
    assert_eq!(refusal(&short).why, Refusal::ExtendedTableSize { len: 16 });
}

#[test]
fn every_update_of_a_file_is_read_and_each_is_whole() {
    let newer = build(T14_SIGNATURE, 0x80, 0xc0, &[]);
    let file = [T14_FILE, &newer[..]].concat();
    let updates = updates(&file).unwrap();
    assert_eq!(updates.len(), 2);
    assert!(updates[0].bytes == T14_FILE && updates[1].bytes == &newer[..], "an update is not its own bytes");
}
