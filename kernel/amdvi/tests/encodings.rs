//! Every encoding against AMD 48882 rev 3.11's own field positions: each
//! expected value is built from `(msb, lsb, value)` triples as the
//! specification's figures and tables give them, and compared whole, so a
//! field in the wrong place and a stray bit are both a difference.

use toyos_amdvi::dte::{Dte, Setting, DEVICE_IDS, ENTRY_BYTES};
use toyos_amdvi::queue::{event, name, Command, Event, ENTRIES};
use toyos_amdvi::reg::{device_table, offset, ring, slot, Features};
use toyos_amdvi::table::{directory, entry, index, leaf, Entry, LEAF_LEVEL, ROOT_LEVEL};
use toyos_phys::Phys;

/// `N` doublewords holding `value` in bits `[msb:lsb]` for every triple, and
/// zero everywhere else. A field may cross a doubleword.
fn layout<const N: usize>(fields: &[(u32, u32, u64)]) -> [u64; N] {
    let mut words = [0u64; N];
    for &(msb, lsb, value) in fields {
        let width = msb - lsb + 1;
        assert!(width == 64 || value >> width == 0, "{value:#x} does not fit [{msb}:{lsb}]");
        for bit in 0..width {
            if value >> bit & 1 != 0 {
                let at = lsb + bit;
                assert_eq!(words[(at / 64) as usize] >> (at % 64) & 1, 0, "two fields claim bit {at}");
                words[(at / 64) as usize] |= 1 << (at % 64);
            }
        }
    }
    words
}

fn word(fields: &[(u32, u32, u64)]) -> u64 {
    layout::<1>(fields)[0]
}

const ROOT: u64 = 0x12_3456_7000;

/// Figure 7: `V` [0], `TV` [1], `Mode` [11:9] 000b, `IR` [61] and `IW` [62]
/// clear, `DomainID` [79:64]; Table 7 bits 11:9 make every access the IR/IW
/// pair's to allow, and both are clear.
#[test]
fn a_blocked_entry_is_valid_untranslated_and_grants_nothing() {
    let want = layout::<2>(&[(0, 0, 1), (1, 1, 1), (79, 64, 0)]);
    assert_eq!(Dte::blocked(0, Setting(0)).words(), want);
}

/// Figure 7: the root at [51:12], `Mode` 100b, `IR` and `IW` set, the domain
/// at [79:64].
#[test]
fn a_translated_entry_names_its_four_level_root_and_domain() {
    let want = layout::<2>(&[(0, 0, 1), (1, 1, 1), (11, 9, 0b100), (51, 12, ROOT >> 12), (61, 61, 1), (62, 62, 1), (79, 64, 0xBEEF)]);
    let root = Phys::new(ROOT).expect("aligned");
    assert_eq!(Dte::translated(root, 0xBEEF, Setting(0)).words(), want);
    assert_eq!(Dte::domain_of(want), 0xBEEF);
}

/// Table 106 puts `SysMgt` at bits 5:4 of the IVHD byte, Table 7 at DTE bits
/// 105:104; the pass bits around it (7, 6, 3, 2, 1, 0) reach no translation
/// half.
#[test]
fn the_ivhd_byte_gives_an_entry_its_sys_mgt_alone() {
    let declared = Setting(0b1101_0111);
    let want = layout::<2>(&[(0, 0, 1), (1, 1, 1), (79, 64, 7), (105, 104, 0b01)]);
    assert_eq!(Dte::blocked(7, declared).words(), want);
    let translated = Dte::translated(Phys::new(ROOT).expect("aligned"), 7, declared).words();
    assert_eq!(translated[1], want[1]);
}

/// §2.2.2: 65,536 entries of 256 bits, 2 MiB; MMIO 0000h `Size` [8:0] counts
/// 4 KiB units less one, so 1FFh, the base at [51:12].
#[test]
fn the_device_table_register_names_two_mebibytes() {
    assert_eq!(DEVICE_IDS * ENTRY_BYTES, 2 << 20);
    assert_eq!(device_table(Phys::new(ROOT).expect("aligned")), word(&[(8, 0, 0x1FF), (51, 12, ROOT >> 12)]));
}

/// MMIO 0008h and 0010h: the base at [51:12], length [59:56] 1000b for 256
/// entries; the head and tail registers count 16-byte entries at [18:4].
#[test]
fn a_ring_register_names_a_four_kibibyte_ring() {
    assert_eq!(ring(Phys::new(ROOT).expect("aligned")), word(&[(51, 12, ROOT >> 12), (59, 56, 0b1000)]));
    assert_eq!(ENTRIES * 16, 4096);
    assert_eq!(offset(255), word(&[(18, 4, 255)]));
    assert_eq!(slot(offset(255)), 255);
}

/// Table 15: level 4 indexes by bits 47:39, level 3 by 38:30, level 2 by
/// 29:21.
#[test]
fn each_level_indexes_by_its_nine_bits() {
    let at = word(&[(47, 39, 0x1A5), (38, 30, 0x0F3), (29, 21, 0x15A), (20, 0, 0x1F_FFFF)]);
    assert_eq!((index(at, 4), index(at, 3), index(at, 2)), (0x1A5, 0x0F3, 0x15A));
    assert_eq!((ROOT_LEVEL, LEAF_LEVEL), (4, 2));
}

/// Figure 10 / Table 18: `PR` [0], `NextLevel` [11:9] the level below, the
/// next table at [51:12], `IR` [61], `IW` [62].
#[test]
fn a_directory_entry_names_the_level_below() {
    let below = Phys::new(ROOT).expect("aligned");
    let want = word(&[(0, 0, 1), (11, 9, 3), (51, 12, ROOT >> 12), (61, 61, 1), (62, 62, 1)]);
    assert_eq!(directory(below, 4), want);
    assert_eq!(entry(want), Entry::Directory(below));
    assert_eq!(entry(directory(below, 3)), Entry::Directory(below));
}

/// Figure 9 / Table 17 at level 2: `PR`, `NextLevel` 000b, the page at
/// [51:21] of a 2 MiB-aligned address, `FC` [60], `IR`, `IW`.
#[test]
fn a_leaf_is_a_two_mebibyte_page_read_write_and_coherent() {
    const PAGE: u64 = 0x7_6540_0000;
    let want = word(&[(0, 0, 1), (51, 21, PAGE >> 21), (60, 60, 1), (61, 61, 1), (62, 62, 1)]);
    assert_eq!(leaf(Phys::new(PAGE).expect("aligned")), want);
    assert_eq!(entry(want), Entry::Leaf);
    assert_eq!(entry(want & !1), Entry::Absent);
    assert_eq!(entry(0), Entry::Absent);
}

/// Figures 44, 45, 46 and 51: the code at [63:60] of the first doubleword.
#[test]
fn each_command_is_its_code_and_operands() {
    const STORE: u64 = 0xA_BCDE_F008;
    let wait = Command::CompletionWait { store: Phys::new(STORE).expect("aligned"), data: 0x600D_F00D };
    assert_eq!(wait.words(), [word(&[(0, 0, 1), (51, 3, STORE >> 3), (63, 60, 1)]), 0x600D_F00D]);
    assert_eq!(Command::InvalidateDevice(0x0318).words(), [word(&[(15, 0, 0x0318), (63, 60, 2)]), 0]);
    // `S` [0] and `PDE` [1] of the second doubleword, address [63:12] all ones
    // but bit 63: every address of the domain (§2.4.3's software note).
    assert_eq!(
        Command::InvalidateDomain(0xBEEF).words(),
        [word(&[(47, 32, 0xBEEF), (63, 60, 3)]), word(&[(0, 0, 1), (1, 1, 1), (62, 12, (1 << 51) - 1)])]
    );
    assert_eq!(Command::InvalidateAll.words(), [word(&[(63, 60, 8)]), 0]);
}

/// Figure 56 / Table 57: `DeviceID` [15:0], `D/P` [47:32], `I` [51], `RW`
/// [53], the address in the second doubleword.
#[test]
fn a_page_fault_names_its_requester_domain_address_and_access() {
    let read = [word(&[(15, 0, 0x0303), (47, 32, 2), (52, 52, 1), (63, 60, 2)]), 0x4000_0000_0000];
    assert_eq!(
        event(read),
        Event::PageFault { device: 0x0303, domain: 2, address: 0x4000_0000_0000, write: false, interrupt: false }
    );
    let write = [read[0] | word(&[(53, 53, 1)]), read[1]];
    assert!(matches!(event(write), Event::PageFault { write: true, interrupt: false, .. }));
    let message = [word(&[(15, 0, 0x0303), (51, 51, 1), (63, 60, 2)]), 0xFEE0_0000];
    assert!(matches!(event(message), Event::PageFault { interrupt: true, .. }));
}

/// Figure 62: `DeviceID` [15:0], code 1000b, the address in the second
/// doubleword; every other code is no requester's (Table 42).
#[test]
fn every_other_record_is_named_by_its_code() {
    let request = [word(&[(15, 0, 0x00A2), (63, 60, 0b1000)]), 0xFD_F800_0000];
    assert_eq!(event(request), Event::DeviceRequest { device: 0x00A2, address: 0xFD_F800_0000 });
    for code in [0b0001u64, 0b0011, 0b0100, 0b0101, 0b0110, 0b0111, 0b1001, 0b1111] {
        assert_eq!(event([code << 60 | 0x0303, 0]), Event::Other { code: code as u8 });
    }
    assert_eq!(name(0b0001), "ILLEGAL_DEV_TABLE_ENTRY");
    assert_eq!(name(0b0101), "ILLEGAL_COMMAND_ERROR");
    assert_eq!(name(0b0000), "reserved");
}

/// MMIO 0030h: `IASup` [6], `HATS` [11:10] counting from four levels, 11b
/// reserved. The AMD bench's unit reads 0x206d73ef22254ade.
#[test]
fn the_feature_register_says_invalidate_all_and_host_levels() {
    let bench = Features(0x206d_73ef_2225_4ade);
    assert!(bench.invalidate_all());
    assert_eq!(bench.host_levels(), Some(6));
    assert!(!Features(word(&[(5, 0, 0x3F), (7, 7, 1)])).invalidate_all());
    assert_eq!(Features(0).host_levels(), Some(4));
    assert_eq!(Features(word(&[(11, 10, 0b11)])).host_levels(), None);
}
