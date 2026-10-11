//! The FACS the FADT names, and the Global Lock's two transitions held to
//! ACPI 6.5 §5.2.10.1's own instruction sequences, executed step by step.

mod common;

use common::{reseal, Machine};
use toyos_acpi::{acquire, facs, release, Facs, FacsRefused, Table, OWNED, PENDING, SDT_HEADER_LEN};

const FADT_AT: u64 = 0x7fb7_9000;
const QEMU_FADT: &[u8] = include_bytes!("../fixtures/qemu-11.1.1/facp.bin");
/// Table 5.9: `FIRMWARE_CTRL` and `X_FIRMWARE_CTRL`.
const FIRMWARE_CTRL: usize = 36;
const X_FIRMWARE_CTRL: usize = 132;

fn fadt_naming(narrow: u32, wide: u64) -> Vec<u8> {
    let mut fadt = QEMU_FADT.to_vec();
    fadt[FIRMWARE_CTRL..FIRMWARE_CTRL + 4].copy_from_slice(&narrow.to_le_bytes());
    fadt[X_FIRMWARE_CTRL..X_FIRMWARE_CTRL + 8].copy_from_slice(&wide.to_le_bytes());
    reseal(&mut fadt);
    fadt
}

fn a_facs(len: u32) -> Vec<u8> {
    let mut bytes = vec![0u8; 64];
    bytes[..4].copy_from_slice(b"FACS");
    bytes[4..8].copy_from_slice(&len.to_le_bytes());
    bytes
}

fn decoded(fadt: &[u8], at: u64, structure: &[u8]) -> Result<Facs, FacsRefused> {
    let regions = [(FADT_AT, fadt), (at, structure)];
    let m = Machine { regions: &regions };
    let fadt = Table::open(m, FADT_AT, b"FACP", SDT_HEADER_LEN).expect("the crafted FADT");
    facs(m, &fadt)
}

#[test]
fn the_wide_address_wins_where_it_is_not_zero_and_the_narrow_one_serves_where_it_is() {
    assert!(QEMU_FADT[8] >= 2 && QEMU_FADT.len() >= X_FIRMWARE_CTRL + 8, "the fixture is a revision with X_FIRMWARE_CTRL");
    let structure = a_facs(64);
    assert_eq!(decoded(&fadt_naming(0x1000, 0x2_0000_0040), 0x2_0000_0040, &structure), Ok(Facs { base: 0x2_0000_0040, len: 64, s4bios: false }));
    assert_eq!(decoded(&fadt_naming(0x1000, 0), 0x1000, &structure), Ok(Facs { base: 0x1000, len: 64, s4bios: false }));
    // The narrow address is not read where the wide one names another place.
    assert_eq!(decoded(&fadt_naming(0x1000, 0x2_0000_0040), 0x1000, &structure), Err(FacsRefused::Unmapped(0x2_0000_0040)));
    assert_eq!(decoded(&fadt_naming(0, 0), 0x1000, &structure), Err(FacsRefused::Absent));
}

#[test]
fn a_structure_that_is_no_facs_is_refused_by_name() {
    let fadt = fadt_naming(0x1000, 0);
    let mut unsigned = a_facs(64);
    unsigned[..4].copy_from_slice(b"FACP");
    assert_eq!(decoded(&fadt, 0x1000, &unsigned), Err(FacsRefused::Signature));
    assert_eq!(decoded(&fadt, 0x1000, &a_facs(63)), Err(FacsRefused::Length(63)));
    assert_eq!(decoded(&fadt, 0x1000, &a_facs(u32::MAX)), Err(FacsRefused::Length(u32::MAX)));
    assert_eq!(decoded(&fadt, 0x1000, &a_facs(64)[..63]), Err(FacsRefused::Unmapped(0x1000)));
}

/// Table 5.14: `S4BIOS_F` is bit 0 of the dword at 20, and no other bit of
/// the structure is read as it.
#[test]
fn s4bios_f_is_bit_zero_of_the_flags() {
    let fadt = fadt_naming(0x1000, 0);
    let flagged = |at: usize, byte: u8| {
        let mut structure = a_facs(64);
        structure[at] = byte;
        decoded(&fadt, 0x1000, &structure).expect("a FACS").s4bios
    };
    assert!(flagged(20, 1));
    assert!(flagged(20, 0xFF));
    assert!(!flagged(20, 0xFE), "the flags' other bits");
    for at in [16, 19, 21, 23, 24, 36] {
        assert!(!flagged(at, 0xFF), "the byte at {at}");
    }
}

/// §5.2.10 aligns the FACS on a 64-byte boundary: every base that is not on
/// one is refused, by either address field, and the boundary itself is not.
#[test]
fn a_facs_off_its_sixty_four_byte_boundary_is_refused() {
    for off in [1u32, 2, 4, 8, 16, 32, 60, 63] {
        let at = 0x1000 + off;
        assert_eq!(decoded(&fadt_naming(at, 0), u64::from(at), &a_facs(64)), Err(FacsRefused::Misaligned(u64::from(at))), "narrow, {off} off");
        let wide = 0x2_0000_0000 + u64::from(off);
        assert_eq!(decoded(&fadt_naming(0x1000, wide), wide, &a_facs(64)), Err(FacsRefused::Misaligned(wide)), "wide, {off} off");
    }
    for at in [0x40u32, 0x1000, 0x1040, 0xFFFF_FFC0] {
        assert_eq!(decoded(&fadt_naming(at, 0), u64::from(at), &a_facs(64)), Ok(Facs { base: u64::from(at), len: 64, s4bios: false }), "{at:#x}");
    }
}

/// `AcquireGlobalLock`, an instruction a line: `and edx, not 1`, `bts edx, 1`,
/// `adc edx, 0`, then `cmp dl, 3` and `sbb eax, eax`.
fn spec_acquire(word: u32) -> (u32, bool) {
    let mut edx = word & !1;
    let carry = edx >> 1 & 1;
    edx |= 1 << 1;
    edx = edx.wrapping_add(carry);
    let below = (edx as u8) < 3;
    (edx, below)
}

/// `ReleaseGlobalLock`: `and edx, not 03h`, then `and eax, 1` of the word read.
fn spec_release(word: u32) -> (u32, bool) {
    (word & !3, word & 1 != 0)
}

#[test]
fn both_transitions_are_the_specifications_own_sequences() {
    // Every state of the two bits, under reserved bits clear, set and mixed
    // above the low byte: `cmp dl, 3` reads that byte whole, so the sequence
    // itself answers for a word whose bits 2 to 7 are zero, as Table 5.16
    // reserves them.
    for reserved in [0u32, !0xFF, 0xA5A5_A500] {
        for state in 0..4u32 {
            let word = reserved | state;
            assert_eq!(acquire(word), spec_acquire(word), "acquire from {word:#x}");
            assert_eq!(release(word), spec_release(word), "release from {word:#x}");
        }
    }
    assert_eq!(acquire(0), (OWNED, true));
    assert_eq!(acquire(OWNED), (OWNED | PENDING, false), "a lock the firmware owns is marked pending, not taken");
    assert_eq!(acquire(OWNED | PENDING), (OWNED | PENDING, false));
    assert_eq!(acquire(PENDING), (OWNED, true), "a stale pending bit is cleared by whoever takes a free lock");
    assert_eq!(release(OWNED), (0, false));
    assert_eq!(release(OWNED | PENDING), (0, true), "the firmware asked while it was held, and is owed the signal");
}
