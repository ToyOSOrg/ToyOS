//! The boundary half: bytes no firmware would publish.
//!
//! Every case here is an input the decoder must **refuse by name** — or, where
//! an 8-bit sum genuinely cannot tell, accept and say so. The claim being held
//! is the crate's own: no panic on any input path, no walk that does not
//! terminate, and one [`TableError`] per reason so the caller's log line names
//! what was wrong rather than "malformed".

mod common;

use common::{declare_len, entry, madt, rsdp, sdt, xsdt, Machine};
use toyos_acpi::{
    dsdt_address, ecam_base, find_table, hpet_base, iapc_boot_arch, madt_entries, reset_register,
    rtc_century, Century, MadtEntry, MadtHalt, Phys, Reset, Table, TableError, MADT_ENTRIES,
    MAX_TABLE_LEN,
};

const RSDP_AT: u64 = 0x1_0000;
const XSDT_AT: u64 = 0x2_0000;
const TABLE_AT: u64 = 0x3_0000;

#[test]
fn a_length_shorter_than_the_fixed_part_is_refused_with_both_numbers() {
    let head = rsdp(XSDT_AT, 2, 36);
    let root = xsdt(&[TABLE_AT]);
    let mut t = sdt(b"MCFG", 1, &[0u8; 24]);
    declare_len(&mut t, 40);
    let regions: &[(u64, &[u8])] = &[(RSDP_AT, &head), (XSDT_AT, &root), (TABLE_AT, &t)];
    let m = Machine { regions };

    assert_eq!(
        ecam_base(m, RSDP_AT).err(),
        Some(TableError::Length { declared: 40, needed: 60 }),
        "the walk stops at a table of the right signature it cannot use, and says why"
    );
    assert_eq!(
        Table::open(m, TABLE_AT, b"MCFG", 60).err(),
        Some(TableError::Length { declared: 40, needed: 60 })
    );
}

/// A length under the header itself: the floor is the header, never the
/// caller's `needed`, or the signature read would already be out of bounds.
#[test]
fn a_length_under_the_header_is_refused_even_when_the_caller_needs_nothing() {
    let head = rsdp(XSDT_AT, 2, 36);
    let root = xsdt(&[TABLE_AT]);
    let mut t = sdt(b"HPET", 1, &[0u8; 20]);
    declare_len(&mut t, 8);
    let regions: &[(u64, &[u8])] = &[(RSDP_AT, &head), (XSDT_AT, &root), (TABLE_AT, &t)];
    assert_eq!(
        Table::open(Machine { regions }, TABLE_AT, b"HPET", 0).err(),
        Some(TableError::Length { declared: 8, needed: 36 })
    );
}

#[test]
fn a_length_longer_than_the_mapping_is_refused_before_a_byte_is_read() {
    let head = rsdp(XSDT_AT, 2, 36);
    let root = xsdt(&[TABLE_AT]);
    let mut t = sdt(b"HPET", 1, &[0u8; 20]);
    declare_len(&mut t, 4096);
    let regions: &[(u64, &[u8])] = &[(RSDP_AT, &head), (XSDT_AT, &root), (TABLE_AT, &t)];
    let m = Machine { regions };

    assert_eq!(
        Table::open(m, TABLE_AT, b"HPET", 36).err(),
        Some(TableError::Unmapped { at: TABLE_AT, len: 4096 })
    );
    // The walk skips it rather than ending: an unreadable entry is one entry.
    assert_eq!(hpet_base(m, RSDP_AT).err(), Some(TableError::Absent));
}

#[test]
fn a_length_over_the_ceiling_is_refused_without_walking_it() {
    let head = rsdp(XSDT_AT, 2, 36);
    let root = xsdt(&[TABLE_AT]);
    let mut t = sdt(b"HPET", 1, &[0u8; 20]);
    let over = MAX_TABLE_LEN as u32 + 1;
    declare_len(&mut t, over);
    let regions: &[(u64, &[u8])] = &[(RSDP_AT, &head), (XSDT_AT, &root), (TABLE_AT, &t)];
    assert_eq!(
        Table::open(Machine { regions }, TABLE_AT, b"HPET", 36).err(),
        Some(TableError::Length { declared: over, needed: 36 })
    );
}

#[test]
fn a_table_that_does_not_sum_to_zero_is_refused() {
    let head = rsdp(XSDT_AT, 2, 36);
    let root = xsdt(&[TABLE_AT]);
    let mut t = sdt(b"HPET", 1, &[0u8; 20]);
    t[9] = t[9].wrapping_add(1);
    let regions: &[(u64, &[u8])] = &[(RSDP_AT, &head), (XSDT_AT, &root), (TABLE_AT, &t)];
    assert_eq!(
        Table::open(Machine { regions }, TABLE_AT, b"HPET", 36).err(),
        Some(TableError::Checksum)
    );
}

/// **The non-detection, stated as a test.** An 8-bit sum cannot see two edits
/// that cancel, so a table corrupted that way is accepted and decodes to the
/// corrupted value. Nothing in this crate can do better; a caller that needs
/// more needs a different check.
#[test]
fn two_edits_that_cancel_pass_the_checksum_and_decode_to_the_lie() {
    let head = rsdp(XSDT_AT, 2, 36);
    let root = xsdt(&[TABLE_AT]);
    let mut t = sdt(b"HPET", 1, &[0u8; 20]);
    // The HPET's 64-bit base address, and a byte elsewhere paying for it.
    t[44] = t[44].wrapping_add(0x40);
    t[36] = t[36].wrapping_sub(0x40);
    let regions: &[(u64, &[u8])] = &[(RSDP_AT, &head), (XSDT_AT, &root), (TABLE_AT, &t)];
    assert_eq!(
        hpet_base(Machine { regions }, RSDP_AT),
        Ok(0x40),
        "the sum is intact and the address is not"
    );
}

#[test]
fn a_madt_entry_of_zero_length_halts_the_walk_instead_of_looping() {
    let mut list = entry(0, 8, &[0, 0, 1, 0, 0, 0]);
    list.extend(entry(0, 0, &[]));
    list.extend(entry(0, 8, &[0, 9, 1, 0, 0, 0]));
    let t = madt(&list);
    let head = rsdp(XSDT_AT, 2, 36);
    let root = xsdt(&[TABLE_AT]);
    let regions: &[(u64, &[u8])] = &[(RSDP_AT, &head), (XSDT_AT, &root), (TABLE_AT, &t)];
    let table = find_table(Machine { regions }, RSDP_AT, b"APIC", MADT_ENTRIES).expect("MADT");

    // Bounded, so a walk that stops terminating reds here under this test's
    // own name instead of being killed as a hung run with nothing attached.
    assert_eq!(
        madt_entries(&table).take(8).collect::<Vec<_>>(),
        [
            Ok(MadtEntry::LocalApic { apic_id: 0, enabled: true }),
            Err(MadtHalt { at: 8, declared: 0, list_len: 18 }),
        ],
        "the entry after the halt is unreachable: a list that lied cannot be resynchronised"
    );
}

#[test]
fn a_madt_entry_running_past_the_list_halts_the_walk() {
    let mut list = entry(1, 12, &[0, 0, 0x00, 0x00, 0xc0, 0xfe, 0, 0, 0, 0]);
    list.extend(entry(0, 200, &[]));
    let t = madt(&list);
    let head = rsdp(XSDT_AT, 2, 36);
    let root = xsdt(&[TABLE_AT]);
    let regions: &[(u64, &[u8])] = &[(RSDP_AT, &head), (XSDT_AT, &root), (TABLE_AT, &t)];
    let table = find_table(Machine { regions }, RSDP_AT, b"APIC", MADT_ENTRIES).expect("MADT");

    let walk: Vec<_> = madt_entries(&table).collect();
    assert_eq!(walk.len(), 2);
    assert_eq!(walk[1], Err(MadtHalt { at: 12, declared: 200, list_len: 14 }));
}

#[test]
fn a_madt_entry_too_short_for_its_own_type_is_not_decoded_as_that_type() {
    let t = madt(&entry(1, 6, &[0, 0, 0xff, 0xff]));
    let head = rsdp(XSDT_AT, 2, 36);
    let root = xsdt(&[TABLE_AT]);
    let regions: &[(u64, &[u8])] = &[(RSDP_AT, &head), (XSDT_AT, &root), (TABLE_AT, &t)];
    let table = find_table(Machine { regions }, RSDP_AT, b"APIC", MADT_ENTRIES).expect("MADT");
    assert_eq!(madt_entries(&table).collect::<Vec<_>>(), [Ok(MadtEntry::Other(1))]);
}

#[test]
fn a_trailing_byte_that_cannot_be_an_entry_header_ends_the_walk_quietly() {
    let mut list = entry(0, 8, &[0, 3, 1, 0, 0, 0]);
    list.push(0);
    let t = madt(&list);
    let head = rsdp(XSDT_AT, 2, 36);
    let root = xsdt(&[TABLE_AT]);
    let regions: &[(u64, &[u8])] = &[(RSDP_AT, &head), (XSDT_AT, &root), (TABLE_AT, &t)];
    let table = find_table(Machine { regions }, RSDP_AT, b"APIC", MADT_ENTRIES).expect("MADT");
    assert_eq!(
        madt_entries(&table).collect::<Vec<_>>(),
        [Ok(MadtEntry::LocalApic { apic_id: 3, enabled: true })]
    );
}

#[test]
fn an_xsdt_entry_pointing_at_nothing_is_skipped_and_the_next_one_is_read() {
    let hpet = sdt(b"HPET", 1, &[0u8; 20]);
    let head = rsdp(XSDT_AT, 2, 36);
    // Address 0, an address no region holds, and the real table.
    let root = xsdt(&[0, 0xdead_0000, TABLE_AT]);
    let regions: &[(u64, &[u8])] = &[(RSDP_AT, &head), (XSDT_AT, &root), (TABLE_AT, &hpet)];
    assert_eq!(hpet_base(Machine { regions }, RSDP_AT), Ok(0));

    let empty = xsdt(&[0, 0xdead_0000]);
    let regions: &[(u64, &[u8])] = &[(RSDP_AT, &head), (XSDT_AT, &empty)];
    assert_eq!(hpet_base(Machine { regions }, RSDP_AT).err(), Some(TableError::Absent));
}

#[test]
fn the_root_pointer_is_refused_one_reason_at_a_time() {
    let root = xsdt(&[]);
    let good = rsdp(XSDT_AT, 2, 36);

    let mut wrong_signature = good.clone();
    wrong_signature[0] = b'X';
    let mut broken_v1 = good.clone();
    broken_v1[8] = broken_v1[8].wrapping_add(1);
    let mut broken_extended = good;
    broken_extended[32] = broken_extended[32].wrapping_add(1);
    let acpi_1_0 = rsdp(XSDT_AT, 1, 36);
    let short = rsdp(XSDT_AT, 2, 20);
    let null_xsdt = rsdp(0, 2, 36);

    for (name, head, want) in [
        ("a signature that is not RSD PTR ", &wrong_signature, TableError::BadRsdp),
        ("an ACPI 1.0 part that does not sum", &broken_v1, TableError::BadRsdp),
        ("an extended part that does not sum", &broken_extended, TableError::BadRsdp),
        ("revision 1, which names no XSDT", &acpi_1_0, TableError::NoXsdt),
        (
            "a length that cannot hold XsdtAddress",
            &short,
            TableError::Length { declared: 20, needed: 36 },
        ),
        ("an XSDT address of zero", &null_xsdt, TableError::NoXsdt),
    ] {
        let regions: &[(u64, &[u8])] = &[(RSDP_AT, head), (XSDT_AT, &root)];
        assert_eq!(hpet_base(Machine { regions }, RSDP_AT).err(), Some(want), "{name}");
    }
}

#[test]
fn an_rsdp_address_the_reader_cannot_reach_is_refused() {
    let regions: &[(u64, &[u8])] = &[];
    assert_eq!(hpet_base(Machine { regions }, RSDP_AT).err(), Some(TableError::BadRsdp));
}

/// An ACPI 1.0-length FADT: 116 bytes, no `X_DSDT`, and the century byte still
/// inside it. The 32-bit `DSDT` is what such a table names.
#[test]
fn a_fadt_of_the_acpi_1_0_length_is_read_without_its_x_fields() {
    let mut body = vec![0u8; 80];
    body[40 - 36..44 - 36].copy_from_slice(&0x7ffb_9000u32.to_le_bytes());
    body[108 - 36] = 0x32;
    let t = sdt(b"FACP", 1, &body);
    assert_eq!(t.len(), 116);
    let head = rsdp(XSDT_AT, 2, 36);
    let root = xsdt(&[TABLE_AT]);
    let regions: &[(u64, &[u8])] = &[(RSDP_AT, &head), (XSDT_AT, &root), (TABLE_AT, &t)];
    let m = Machine { regions };

    let fadt = find_table(m, RSDP_AT, b"FACP", 109).expect("FADT");
    assert_eq!(dsdt_address(&fadt), 0x7ffb_9000);
    assert_eq!(rtc_century(m, RSDP_AT), Ok(Century::At(0x32)));
    // The flags word at 109 is inside 116 bytes, so it is *read* — and it comes
    // back with revision 1 beside it, which is the whole of what tells the
    // caller that bit 1 means nothing on this firmware.
    assert_eq!(iapc_boot_arch(m, RSDP_AT), Ok((1, 0)));
}

/// A FADT that stops before the flags word: a refusal, never a zero.
#[test]
fn a_fadt_too_short_for_the_boot_architecture_flags_is_refused() {
    let t = sdt(b"FACP", 1, &[0u8; 73]);
    assert_eq!(t.len(), 109);
    let head = rsdp(XSDT_AT, 2, 36);
    let root = xsdt(&[TABLE_AT]);
    let regions: &[(u64, &[u8])] = &[(RSDP_AT, &head), (XSDT_AT, &root), (TABLE_AT, &t)];
    let m = Machine { regions };
    assert_eq!(
        iapc_boot_arch(m, RSDP_AT).err(),
        Some(TableError::Length { declared: 109, needed: 111 }),
        "a FADT too short for the flags is not a FADT the i8042 driver may read"
    );
    // The century byte at 108 is the last one it does hold.
    assert_eq!(rtc_century(m, RSDP_AT), Ok(Century::Absent));
}

#[test]
fn a_revision_that_promises_x_dsdt_over_a_table_that_cannot_hold_it_falls_back() {
    let mut body = vec![0u8; 80];
    body[40 - 36] = 0x11;
    let t = sdt(b"FACP", 3, &body);
    let head = rsdp(XSDT_AT, 2, 36);
    let root = xsdt(&[TABLE_AT]);
    let regions: &[(u64, &[u8])] = &[(RSDP_AT, &head), (XSDT_AT, &root), (TABLE_AT, &t)];
    let fadt = find_table(Machine { regions }, RSDP_AT, b"FACP", 109).expect("FADT");
    assert_eq!(dsdt_address(&fadt), 0x11);
}

/// The revision gate, over a table long enough for `X_DSDT` to be read if the
/// gate were not there: `X_DSDT` is defined from FADT revision 2, so at
/// revision 1 those eight bytes are whatever the firmware left in a reserved
/// field and the 32-bit `DSDT` is the only address in the table.
#[test]
fn a_revision_1_fadt_long_enough_to_hold_x_dsdt_is_still_read_at_its_32_bit_field() {
    let mut body = vec![0u8; 208];
    body[40 - 36..44 - 36].copy_from_slice(&0x7ffb_9000u32.to_le_bytes());
    body[140 - 36..148 - 36].copy_from_slice(&0xdead_beef_0000_u64.to_le_bytes());
    let t = sdt(b"FACP", 1, &body);
    assert_eq!(t.len(), 244);
    let head = rsdp(XSDT_AT, 2, 36);
    let root = xsdt(&[TABLE_AT]);
    let regions: &[(u64, &[u8])] = &[(RSDP_AT, &head), (XSDT_AT, &root), (TABLE_AT, &t)];
    let fadt = find_table(Machine { regions }, RSDP_AT, b"FACP", 109).expect("FADT");
    assert_eq!(
        dsdt_address(&fadt),
        0x7ffb_9000,
        "revision 1 does not define X_DSDT, so the qword at 140 is not an address"
    );

    // The same bytes at revision 2, where the field is defined: now it is read.
    let t = sdt(b"FACP", 2, &body);
    let regions: &[(u64, &[u8])] = &[(RSDP_AT, &head), (XSDT_AT, &root), (TABLE_AT, &t)];
    let fadt = find_table(Machine { regions }, RSDP_AT, b"FACP", 109).expect("FADT");
    assert_eq!(dsdt_address(&fadt), 0xdead_beef_0000);
}

#[test]
fn a_century_register_outside_cmos_ram_is_told_apart_from_none() {
    for (byte, want) in [
        (0u8, Century::Absent),
        (0x0d, Century::OutOfRange(0x0d)),
        (0x80, Century::OutOfRange(0x80)),
        (0xff, Century::OutOfRange(0xff)),
        (0x0e, Century::At(0x0e)),
        (0x7f, Century::At(0x7f)),
    ] {
        let mut body = vec![0u8; 80];
        body[108 - 36] = byte;
        let t = sdt(b"FACP", 1, &body);
        let head = rsdp(XSDT_AT, 2, 36);
        let root = xsdt(&[TABLE_AT]);
        let regions: &[(u64, &[u8])] = &[(RSDP_AT, &head), (XSDT_AT, &root), (TABLE_AT, &t)];
        assert_eq!(rtc_century(Machine { regions }, RSDP_AT), Ok(want), "century byte {byte:#04x}");
    }
}

const FIXTURES: &[(&str, &[u8], u64)] = &[
    ("rsdp", include_bytes!("../fixtures/qemu-11.1.0/rsdp.bin"), 0x7fb7_e014),
    ("xsdt", include_bytes!("../fixtures/qemu-11.1.0/xsdt.bin"), 0x7fb7_d0e8),
    ("facp", include_bytes!("../fixtures/qemu-11.1.0/facp.bin"), 0x7fb7_9000),
    ("apic", include_bytes!("../fixtures/qemu-11.1.0/apic.bin"), 0x7fb7_8000),
    ("hpet", include_bytes!("../fixtures/qemu-11.1.0/hpet.bin"), 0x7fb7_7000),
    ("mcfg", include_bytes!("../fixtures/qemu-11.1.0/mcfg.bin"), 0x7fb7_6000),
    ("dmar", include_bytes!("../fixtures/qemu-11.1.0/dmar.bin"), 0x7fb7_5000),
    ("waet", include_bytes!("../fixtures/qemu-11.1.0/waet.bin"), 0x7fb7_4000),
];

/// **No panic and no unbounded walk, over every byte of every table this crate
/// decodes.** Each byte of each fixture takes each of its 255 other values in
/// turn, and `ecam_base`, `hpet_base`, `rtc_century`, `iapc_boot_arch` and the
/// MADT walk to exhaustion all run over it. A panic anywhere — including the
/// reader's own, which fires on a read no bound accepted — reds this.
///
/// **Both arms, and the second is the one that reaches the decode.** A raw
/// single-byte edit breaks the table's own 8-bit sum, so the checksum refuses
/// it before any field is read: measured, that arm walked *no* MADT at all and
/// the counter below is what says so. The resealed arm re-sums the table after
/// the edit — which is what a hostile firmware would do — and is the only way
/// a length, a count or an entry header under test is ever reached.
#[test]
fn no_single_byte_mutation_of_a_real_table_panics_or_runs_away() {
    let rsdp_at = FIXTURES[0].2;
    let mut mutations = 0u64;
    let mut walked = [0u64; 2];
    let mut halts = [0u64; 2];

    for reseal in [false, true] {
        let arm = usize::from(reseal);
        for (i, (which, original, _)) in FIXTURES.iter().enumerate() {
            for offset in 0..original.len() {
                for value in 0..=255u8 {
                    if original[offset] == value {
                        continue;
                    }
                    let mut mutated = original.to_vec();
                    mutated[offset] = value;
                    if reseal {
                        if *which == "rsdp" {
                            common::reseal_rsdp(&mut mutated);
                        } else {
                            common::reseal(&mut mutated);
                        }
                    }
                    let regions: Vec<(u64, &[u8])> = FIXTURES
                        .iter()
                        .enumerate()
                        .map(|(j, (_, b, at))| (*at, if j == i { mutated.as_slice() } else { *b }))
                        .collect();
                    let m = Machine { regions: &regions };

                    let _ = ecam_base(m, rsdp_at);
                    let _ = hpet_base(m, rsdp_at);
                    let _ = rtc_century(m, rsdp_at);
                    let _ = iapc_boot_arch(m, rsdp_at);
                    if let Ok(t) = find_table(m, rsdp_at, b"FACP", 36) {
                        let _ = reset_register(&t);
                    }
                    if let Ok(t) = find_table(m, rsdp_at, b"APIC", MADT_ENTRIES) {
                        // Bounded by the table's own length, so a walk that has
                        // stopped advancing reds here instead of hanging: every
                        // entry is at least two bytes.
                        let mut seen = 0usize;
                        for item in madt_entries(&t) {
                            seen += 1;
                            assert!(
                                seen <= t.len(),
                                "{which}[{offset}]={value:#04x}: the MADT walk is not advancing"
                            );
                            if item.is_err() {
                                halts[arm] += 1;
                                break;
                            }
                        }
                        walked[arm] += seen as u64;
                    }
                    mutations += 1;
                }
            }
        }
    }
    assert!(mutations > 300_000, "only {mutations} mutations were run");
    assert_eq!(
        halts[0], 0,
        "an unsealed single-byte edit reached a MADT walk, so the checksum is not refusing it \
         first and the second arm is measuring nothing new"
    );
    assert!(walked[1] > walked[0], "the resealed arm walked no further than the raw one");
    assert!(halts[1] > 0, "no resealed mutation halted a walk, so that arm is untested here");
}

/// **Stated as tests, so extending the decoder reds the statement.** Nothing in
/// this crate reads what is *inside* a DSDT — `find_s5_slp_typ` scans AML and
/// stays in the kernel — and the XSDT walk deliberately returns the first
/// signature match's verdict rather than trying a second table of the same name.
#[test]
fn the_two_things_the_corpus_does_not_cover_are_the_two_the_crate_does_not_do() {
    let head = rsdp(XSDT_AT, 2, 36);
    let dsdt = sdt(b"DSDT", 2, &[0u8; 8]);
    let root = xsdt(&[TABLE_AT]);
    let regions: &[(u64, &[u8])] = &[(RSDP_AT, &head), (XSDT_AT, &root), (TABLE_AT, &dsdt)];
    let m = Machine { regions };
    // A DSDT opens like any other table; its contents are nothing this crate
    // reads, so no case here covers `\_S5_`.
    assert!(Table::open(m, TABLE_AT, b"DSDT", 36).is_ok());
    assert_eq!(hpet_base(m, RSDP_AT).err(), Some(TableError::Absent));

    let mut broken = sdt(b"HPET", 1, &[0u8; 20]);
    broken[9] = broken[9].wrapping_add(1);
    let good = sdt(b"HPET", 1, &[0u8; 20]);
    let root = xsdt(&[TABLE_AT, TABLE_AT + 0x1000]);
    let regions: &[(u64, &[u8])] =
        &[(RSDP_AT, &head), (XSDT_AT, &root), (TABLE_AT, &broken), (TABLE_AT + 0x1000, &good)];
    assert_eq!(
        hpet_base(Machine { regions }, RSDP_AT).err(),
        Some(TableError::Checksum),
        "the second HPET is never reached, and that is the decode's choice"
    );
}

#[test]
fn the_shared_reader_refuses_what_it_does_not_hold() {
    let bytes = [1u8, 2, 3, 4];
    let regions: &[(u64, &[u8])] = &[(0x100, &bytes)];
    let m = Machine { regions };
    assert!(m.readable(0x100, 4));
    assert!(!m.readable(0x100, 5));
    assert!(!m.readable(0xff, 1));
    assert_eq!(m.byte(0x102), 3);
}

/// A revision-`rev` FADT declaring `flags`, `gas` and `value`. Table offsets are absolute, so each body index is one less the 36-byte header.
fn facp(rev: u8, flags: u32, gas: [u8; 12], value: u8) -> Vec<u8> {
    let mut body = vec![0u8; 129 - 36];
    body[112 - 36..116 - 36].copy_from_slice(&flags.to_le_bytes());
    body[116 - 36..128 - 36].copy_from_slice(&gas);
    body[128 - 36] = value;
    sdt(b"FACP", rev, &body)
}

fn gas(space: u8, bit_width: u8, bit_offset: u8, address: u64) -> [u8; 12] {
    let mut g = [0u8; 12];
    g[0] = space;
    g[1] = bit_width;
    g[2] = bit_offset;
    g[4..].copy_from_slice(&address.to_le_bytes());
    g
}

fn reset_of(table: &[u8]) -> Reset {
    let regions: &[(u64, &[u8])] = &[(TABLE_AT, table)];
    let fadt = Table::open(Machine { regions }, TABLE_AT, b"FACP", 36).expect("a sealed FADT");
    reset_register(&fadt)
}

#[test]
fn a_reset_register_this_kernel_cannot_write_is_refused_by_the_field_that_refused_it() {
    const SUP: u32 = 1 << 10;
    let io = gas(1, 8, 0, 0xcf9);

    assert_eq!(reset_of(&facp(3, SUP, io, 0x0f)), Reset::Port { port: 0xcf9, value: 0x0f });
    assert_eq!(reset_of(&facp(3, 0, io, 0x0f)), Reset::Unsupported);
    assert_eq!(reset_of(&facp(3, SUP, gas(0, 8, 0, 0xcf9), 0x0f)), Reset::SystemMemory);
    assert_eq!(reset_of(&facp(3, SUP, gas(2, 8, 0, 0xcf9), 0x0f)), Reset::PciConfig);
    assert_eq!(reset_of(&facp(3, SUP, gas(4, 8, 0, 0xcf9), 0x0f)), Reset::Space(4));
    assert_eq!(
        reset_of(&facp(3, SUP, gas(1, 32, 0, 0xcf9), 0x0f)),
        Reset::Field { bit_width: 32, bit_offset: 0 }
    );
    assert_eq!(
        reset_of(&facp(3, SUP, gas(1, 8, 2, 0xcf9), 0x0f)),
        Reset::Field { bit_width: 8, bit_offset: 2 }
    );
    assert_eq!(reset_of(&facp(3, SUP, gas(1, 8, 0, 0x1_0000), 0x0f)), Reset::Address(0x1_0000));
    assert_eq!(reset_of(&facp(3, SUP, gas(1, 8, 0, 0), 0x0f)), Reset::Address(0));
}

/// Below revision 3 those offsets are reserved bytes, and decoding them yields a port firmware never named.
#[test]
fn reset_fields_are_read_only_from_a_revision_that_defines_them() {
    const SUP: u32 = 1 << 10;
    let io = gas(1, 8, 0, 0xcf9);
    assert_eq!(reset_of(&facp(1, SUP, io, 0x0f)), Reset::Absent);
    assert_eq!(reset_of(&facp(2, SUP, io, 0x0f)), Reset::Absent);

    let mut short = facp(3, SUP, io, 0x0f);
    declare_len(&mut short, 128);
    assert_eq!(reset_of(&short), Reset::Absent);
}
