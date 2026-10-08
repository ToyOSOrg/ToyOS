//! The boundary half: bytes no firmware would publish.
//!
//! Every case here is an input the decoder must **refuse by name** — or, where
//! an 8-bit sum genuinely cannot tell, accept and say so. The claim being held
//! is the crate's own: no panic on any input path, no walk that does not
//! terminate, and one named refusal per reason so the caller's log line says
//! what was wrong rather than "malformed".

mod common;

use common::{declare_len, entry, madt, rsdp, sdt, t14_root_bridge, xsdt, Machine, OVMF_ROOT_BRIDGE};
use toyos_abi::boot::RootBridgeWindow;
use toyos_abi::acpi::Block;
use toyos_acpi::{
    definition_blocks, dsdt_address, ecam_base, ecdt, find_table, fixed_hardware, hpet_base, iapc_boot_arch, isa_line,
    madt_entries, memory_windows, pm1a_control, psci, reset_register, rtc_century, sci_line, Century,
    EcRefused, Field, FixedRefused, LegacyMode, Line, MadtEntry, MadtHalt, Phys, Polarity, PowerButton, Psci, SmiCmd,
    Register, Reset, SourceOverride, Table, TableError, Trigger, ECDT_NEEDED,
    FADT_FOR_FIXED_HARDWARE, MADT_ENTRIES, MAX_TABLE_LEN,
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
    ("rsdp", include_bytes!("../fixtures/qemu-11.1.1/rsdp.bin"), 0x7fb7_e014),
    ("xsdt", include_bytes!("../fixtures/qemu-11.1.1/xsdt.bin"), 0x7fb7_d0e8),
    ("facp", include_bytes!("../fixtures/qemu-11.1.1/facp.bin"), 0x7fb7_9000),
    ("apic", include_bytes!("../fixtures/qemu-11.1.1/apic.bin"), 0x7fb7_8000),
    ("hpet", include_bytes!("../fixtures/qemu-11.1.1/hpet.bin"), 0x7fb7_7000),
    ("mcfg", include_bytes!("../fixtures/qemu-11.1.1/mcfg.bin"), 0x7fb7_6000),
    ("dmar", include_bytes!("../fixtures/qemu-11.1.1/dmar.bin"), 0x7fb7_5000),
    ("waet", include_bytes!("../fixtures/qemu-11.1.1/waet.bin"), 0x7fb7_4000),
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
                    if let Ok(blocks) = definition_blocks(m, rsdp_at) {
                        // The DSDT, and at most an item an 8-byte entry of the largest XSDT.
                        assert!(blocks.count() <= 1 + MAX_TABLE_LEN / 8, "{which}[{offset}]={value:#04x}: the block walk is not ending");
                    }
                    if let Ok(t) = find_table(m, rsdp_at, b"FACP", 36) {
                        let _ = reset_register(&t);
                        let _ = psci(&t);
                        let _ = fixed_hardware(&t);
                        let _ = pm1a_control(&t);
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

/// A FADT of the ACPI 1.0 length naming its DSDT at `dsdt`.
fn facp_naming(dsdt: u32) -> Vec<u8> {
    let mut body = vec![0u8; 116 - 36];
    body[40 - 36..44 - 36].copy_from_slice(&dsdt.to_le_bytes());
    sdt(b"FACP", 1, &body)
}

/// The DSDT first, wherever the XSDT lists the FADT, then the SSDTs in the
/// XSDT's order and nothing else it lists; a refused block is answered in
/// its place and the walk goes on past it.
#[test]
fn the_definition_blocks_are_the_dsdt_and_then_each_ssdt_in_the_xsdts_order() {
    const DSDT_AT: u64 = 0x10_0000;
    let at = |n: u64| TABLE_AT + n * 0x1000;
    let head = rsdp(XSDT_AT, 2, 36);
    let fadt = facp_naming(DSDT_AT as u32);
    let dsdt = sdt(b"DSDT", 2, &[1]);
    let (first, second, third) = (sdt(b"SSDT", 2, &[1]), sdt(b"SSDT", 2, &[2, 2]), sdt(b"SSDT", 2, &[3, 3, 3]));
    let mut broken = sdt(b"SSDT", 2, &[4]);
    broken[9] = broken[9].wrapping_add(1);
    let mut long = sdt(b"SSDT", 2, &[5]);
    declare_len(&mut long, 0x2000);
    let hpet = sdt(b"HPET", 1, &[0u8; 20]);
    // An SSDT ahead of the FADT, a null entry, a table that is no definition
    // block, an SSDT that does not sum, an entry no region holds, an SSDT
    // longer than what holds it, and a DSDT the XSDT lists itself, which is
    // not where a DSDT is named.
    let root = xsdt(&[at(1), 0, at(0), at(6), at(2), at(3), 0xdead_0000, at(4), DSDT_AT, at(5)]);
    let regions: &[(u64, &[u8])] = &[
        (RSDP_AT, &head),
        (XSDT_AT, &root),
        (at(0), &fadt),
        (at(1), &first),
        (at(2), &second),
        (at(3), &broken),
        (at(4), &long),
        (at(5), &third),
        (at(6), &hpet),
        (DSDT_AT, &dsdt),
    ];
    let blocks: Vec<Result<(u64, usize), TableError>> = definition_blocks(Machine { regions }, RSDP_AT)
        .expect("the XSDT")
        .map(|block| block.map(|table| (table.base(), table.len())))
        .collect();
    assert_eq!(
        blocks,
        [
            Ok((DSDT_AT, 37)),
            Ok((at(1), 37)),
            Ok((at(2), 38)),
            Err(TableError::Checksum),
            Err(TableError::Unmapped { at: 0xdead_0000, len: 36 }),
            Err(TableError::Unmapped { at: at(4), len: 0x2000 }),
            Ok((at(5), 39)),
        ]
    );
}

/// The DSDT's item is the first whatever became of it: a machine whose XSDT
/// lists no FADT, and one whose FADT names no DSDT, each say so there, and
/// the SSDTs follow all the same.
#[test]
fn a_dsdt_that_cannot_be_found_is_the_first_block_refused() {
    let head = rsdp(XSDT_AT, 2, 36);
    let ssdt = sdt(b"SSDT", 2, &[1]);
    let walk = |regions: &[(u64, &[u8])]| -> Vec<Result<u64, TableError>> {
        definition_blocks(Machine { regions }, RSDP_AT).expect("the XSDT").map(|block| block.map(|table| table.base())).collect()
    };

    let root = xsdt(&[TABLE_AT]);
    assert_eq!(walk(&[(RSDP_AT, &head), (XSDT_AT, &root), (TABLE_AT, &ssdt)]), [Err(TableError::Absent), Ok(TABLE_AT)]);

    let fadt = facp_naming(0);
    let root = xsdt(&[TABLE_AT + 0x1000, TABLE_AT]);
    assert_eq!(
        walk(&[(RSDP_AT, &head), (XSDT_AT, &root), (TABLE_AT, &ssdt), (TABLE_AT + 0x1000, &fadt)]),
        [Err(TableError::Unmapped { at: 0, len: 36 }), Ok(TABLE_AT)]
    );

    // The FADT names a table that is there and is no DSDT.
    let fadt = facp_naming(TABLE_AT as u32);
    let root = xsdt(&[TABLE_AT + 0x1000]);
    assert_eq!(walk(&[(RSDP_AT, &head), (XSDT_AT, &root), (TABLE_AT, &ssdt), (TABLE_AT + 0x1000, &fadt)]), [Err(TableError::Absent)]);

    assert_eq!(definition_blocks(Machine { regions: &[] }, RSDP_AT).err(), Some(TableError::BadRsdp));
}

/// A machine whose reader reaches its RSDP, its XSDT and one table that is
/// no definition block, and none of the memory its FADT, DSDT and SSDTs are
/// in. The DSDT is refused as the first entry that could not be read, which
/// may be the FADT: `Absent` there would say the firmware names no DSDT,
/// where what happened is that its tables' memory was not read. Each other
/// unread entry follows in its place, and the table that was read and is no
/// SSDT is passed over.
#[test]
fn a_dsdt_whose_fadt_could_not_be_read_is_refused_as_unread_and_not_as_absent() {
    let at = |n: u64| TABLE_AT + n * 0x1000;
    let head = rsdp(XSDT_AT, 2, 36);
    let hpet = sdt(b"HPET", 1, &[0u8; 20]);
    let root = xsdt(&[at(0), at(1), at(2), 0, at(3)]);
    let regions: &[(u64, &[u8])] = &[(RSDP_AT, &head), (XSDT_AT, &root), (at(2), &hpet)];
    let blocks: Vec<Option<TableError>> =
        definition_blocks(Machine { regions }, RSDP_AT).expect("the XSDT").map(|block| block.err()).collect();
    let unread = |n| Some(TableError::Unmapped { at: at(n), len: 36 });
    assert_eq!(blocks, [unread(0), unread(0), unread(1), unread(3)]);

    // Every entry read, and none a FADT: that is `Absent`.
    let root = xsdt(&[at(2), 0]);
    let regions: &[(u64, &[u8])] = &[(RSDP_AT, &head), (XSDT_AT, &root), (at(2), &hpet)];
    let blocks: Vec<Option<TableError>> =
        definition_blocks(Machine { regions }, RSDP_AT).expect("the XSDT").map(|block| block.err()).collect();
    assert_eq!(blocks, [Some(TableError::Absent)]);

    // A FADT that is read wins over an entry before it that was not.
    let fadt = facp_naming(at(4) as u32);
    let dsdt = sdt(b"DSDT", 2, &[1]);
    let root = xsdt(&[at(0), at(1)]);
    let regions: &[(u64, &[u8])] = &[(RSDP_AT, &head), (XSDT_AT, &root), (at(1), &fadt), (at(4), &dsdt)];
    let blocks: Vec<Result<u64, TableError>> =
        definition_blocks(Machine { regions }, RSDP_AT).expect("the XSDT").map(|block| block.map(|table| table.base())).collect();
    assert_eq!(blocks, [Ok(at(4)), Err(TableError::Unmapped { at: at(0), len: 36 })]);
}

/// **Stated as a test, so extending the decoder reds the statement.** The XSDT
/// walk deliberately returns the first signature match's verdict rather than
/// trying a second table of the same name.
#[test]
fn the_xsdt_walk_answers_with_its_first_match_and_tries_no_second() {
    let head = rsdp(XSDT_AT, 2, 36);
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

/// A revision-`rev` FADT carrying `ARM_BOOT_ARCH` `flags` at 129 and `FADT
/// Minor Version` `minor` at 131.
fn arm_facp(rev: u8, minor: u8, flags: u16) -> Vec<u8> {
    let mut body = vec![0u8; 132 - 36];
    body[129 - 36..131 - 36].copy_from_slice(&flags.to_le_bytes());
    body[131 - 36] = minor;
    sdt(b"FACP", rev, &body)
}

fn psci_of(table: &[u8]) -> Psci {
    let regions: &[(u64, &[u8])] = &[(TABLE_AT, table)];
    let fadt = Table::open(Machine { regions }, TABLE_AT, b"FACP", 36).expect("a sealed FADT");
    psci(&fadt)
}

/// `ARM_BOOT_ARCH` on a FADT that defines it.
#[test]
fn the_psci_conduit_is_read_off_a_fadt_that_defines_it() {
    assert_eq!(psci_of(&arm_facp(6, 3, 0b01)), Psci::Smc);
    assert_eq!(psci_of(&arm_facp(6, 3, 0b11)), Psci::Hvc);
    assert_eq!(psci_of(&arm_facp(6, 3, 0b10)), Psci::Absent, "USE_HVC without COMPLIANT is no PSCI");
    assert_eq!(psci_of(&arm_facp(6, 3, 0)), Psci::Absent);
    assert_eq!(psci_of(&arm_facp(5, 1, 0b01)), Psci::Smc, "5.1 is the first version with the field");
}

/// Before ACPI 5.1 the three bytes are reserved, and flags written there are
/// not read; the minor version is the low nibble, the high one an errata letter.
#[test]
fn a_fadt_before_acpi_5_1_says_nothing_about_psci() {
    assert_eq!(psci_of(&arm_facp(5, 0, 0b11)), Psci::Undefined { revision: 5, minor: 0 });
    assert_eq!(psci_of(&arm_facp(3, 0, 0b01)), Psci::Undefined { revision: 3, minor: 0 });
    assert_eq!(psci_of(&arm_facp(5, 0x10, 0b01)), Psci::Undefined { revision: 5, minor: 0 });
}

/// A table that ends inside `ARM_BOOT_ARCH`, or before the minor version that
/// says whether it has one, is short whatever its revision says.
#[test]
fn a_fadt_that_ends_before_arm_boot_arch_is_short() {
    for len in [130, 131] {
        let mut short = arm_facp(6, 3, 0b01);
        declare_len(&mut short, len);
        assert_eq!(psci_of(&short), Psci::Short, "a FADT of {len} bytes");
    }
}

/// Where the two firmwares' descriptor lists sit for the sweep below.
const ROOT_BRIDGE_AT: u64 = 0x4_0000;

/// **No panic and no unbounded walk, over every byte of both firmwares' real
/// descriptor lists.** Each byte takes each of its 255 other values in turn and
/// the whole list is decoded over it. A resource list carries no checksum,
/// unlike the tables above, so a single byte reaches the decode directly.
#[test]
fn no_single_byte_mutation_of_a_firmwares_descriptor_list_panics_or_runs_away() {
    let mut mutations = 0u64;
    let mut refused = 0u64;
    for (which, original) in [("ovmf", OVMF_ROOT_BRIDGE.to_vec()), ("thinkpad-t14", t14_root_bridge())] {
        for offset in 0..original.len() {
            for value in 0..=255u8 {
                if original[offset] == value {
                    continue;
                }
                let mut mutated = original.to_vec();
                mutated[offset] = value;
                let regions: &[(u64, &[u8])] = &[(ROOT_BRIDGE_AT, &mutated)];
                let mut out = [RootBridgeWindow::default(); 8];
                match memory_windows(Machine { regions }, ROOT_BRIDGE_AT, &mut out) {
                    Ok(count) => assert!(
                        out[..count].iter().all(|w| w.length != 0),
                        "{which} byte {offset} as {value:#04x}: a window of no length was carried"
                    ),
                    Err(_) => refused += 1,
                }
                mutations += 1;
            }
        }
    }
    // Both arms are reached: a sweep that only ever refused, or only ever
    // decoded, would be measuring one path.
    assert_eq!(mutations, 2 * 186 * 255);
    assert!(refused > 0 && refused < mutations, "{refused} of {mutations} refused");
}

/// A revision-6 FADT of 276 bytes naming q35's fixed hardware in both forms,
/// for a test to break one field of.
fn crafted(edit: impl FnOnce(&mut [u8])) -> Vec<u8> {
    let mut t = vec![0u8; 276];
    let mut put = |at: usize, bytes: &[u8]| t[at..at + bytes.len()].copy_from_slice(bytes);
    put(46, &[9, 0]);
    put(48, &0xb2u32.to_le_bytes());
    put(52, &[2, 3]);
    put(56, &0x600u32.to_le_bytes());
    put(64, &0x604u32.to_le_bytes());
    put(80, &0x620u32.to_le_bytes());
    put(88, &[4, 2, 0, 4, 16, 0]);
    let gas = |address: u16| {
        let mut g = [1u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        g[4..6].copy_from_slice(&address.to_le_bytes());
        g
    };
    put(148, &gas(0x600));
    put(172, &gas(0x604));
    put(220, &gas(0x620));
    edit(&mut t);
    sdt(b"FACP", 6, &t[36..])
}

fn fixed(edit: impl FnOnce(&mut [u8])) -> Result<toyos_acpi::FixedHardware, FixedRefused> {
    let t = crafted(edit);
    let regions: &[(u64, &[u8])] = &[(TABLE_AT, &t)];
    fixed_hardware(&Table::open(Machine { regions }, TABLE_AT, b"FACP", FADT_FOR_FIXED_HARDWARE).expect("FADT"))
}

fn control(edit: impl FnOnce(&mut [u8])) -> Result<Block, FixedRefused> {
    let t = crafted(edit);
    let regions: &[(u64, &[u8])] = &[(TABLE_AT, &t)];
    pm1a_control(&Table::open(Machine { regions }, TABLE_AT, b"FACP", FADT_FOR_FIXED_HARDWARE).expect("FADT"))
}

#[test]
fn the_crafted_fixed_hardware_decodes_before_any_field_is_broken() {
    let decoded = fixed(|_| {}).expect("the unbroken table");
    assert_eq!(decoded.pm1a_event, Block { port: 0x600, len: 4 });
    assert_eq!(control(|_| {}), Ok(Block { port: 0x604, len: 2 }));
    assert_eq!(decoded.gpe0, Block { port: 0x620, len: 16 });
    assert_eq!(decoded.power_button, PowerButton::Fixed);
}

/// One field broken at a time, each refused by its own name.
#[test]
fn fixed_hardware_this_kernel_does_not_serve_is_refused_by_name() {
    let reduced = fixed(|t| t[112..116].copy_from_slice(&(1u32 << 20).to_le_bytes()));
    assert_eq!(reduced, Err(FixedRefused::HardwareReduced));
    assert_eq!(fixed(|t| t[60] = 0x40), Err(FixedRefused::Pm1b), "PM1b's event block");
    assert_eq!(fixed(|t| t[184 + 4] = 0x40), Err(FixedRefused::Pm1b), "PM1b's control block in its X_ field alone");
    assert_eq!(fixed(|t| t[93] = 8), Err(FixedRefused::Gpe1), "a GPE1 length");
    assert_eq!(
        fixed(|t| t[148] = 0),
        Err(FixedRefused::NotSystemIo { field: Field::Pm1aEvent, space: 0 }),
        "a PM1a event block in memory"
    );
    assert_eq!(
        control(|t| t[172 + 4] = 0x08),
        Err(FixedRefused::Disagrees { field: Field::Pm1aControl, legacy: 0x604, extended: 0x608 })
    );
    assert_eq!(fixed(|t| t[88] = 2), Err(FixedRefused::Length { field: Field::Pm1aEvent, len: 2 }));
    assert_eq!(control(|t| t[89] = 0), Err(FixedRefused::Length { field: Field::Pm1aControl, len: 0 }));
    let nowhere = |t: &mut [u8], legacy: usize, x: usize| {
        t[legacy..legacy + 4].fill(0);
        t[x + 4..x + 12].fill(0);
    };
    assert_eq!(control(|t| nowhere(t, 64, 172)), Err(FixedRefused::Absent { field: Field::Pm1aControl }));
    assert_eq!(fixed(|t| nowhere(t, 56, 148)), Err(FixedRefused::Absent { field: Field::Pm1aEvent }));
    assert_eq!(fixed(|t| t[92] = 5), Err(FixedRefused::Length { field: Field::Gpe0, len: 5 }));
    assert_eq!(
        fixed(|t| {
            t[80..84].copy_from_slice(&0xfff8u32.to_le_bytes());
            t[220 + 4..220 + 6].copy_from_slice(&0xfff8u16.to_le_bytes());
        }),
        Err(FixedRefused::PastPorts { field: Field::Gpe0, address: 0xfff8 }),
        "sixteen bytes from 0xfff8"
    );
    assert_eq!(fixed(|t| t[48..52].copy_from_slice(&0x1_0000u32.to_le_bytes())), Err(FixedRefused::SmiCmd(0x1_0000)));
}

/// What a firmware may leave out and still be served: an `X_` field naming no
/// address, a 32-bit field left zero beside an `X_` one, no GPE0 block, no
/// `SMI_CMD`, and a control-method power button said as one.
#[test]
fn fixed_hardware_reads_whichever_form_names_a_block() {
    let x_only = fixed(|t| t[56..60].copy_from_slice(&[0; 4])).expect("an X_ field alone");
    assert_eq!(x_only.pm1a_event, Block { port: 0x600, len: 4 });
    let legacy_only = fixed(|t| t[148 + 4..148 + 12].copy_from_slice(&[0; 8])).expect("a 32-bit field alone");
    assert_eq!(legacy_only.pm1a_event, Block { port: 0x600, len: 4 });
    let no_gpe = fixed(|t| {
        t[80..84].copy_from_slice(&[0; 4]);
        t[220 + 4..220 + 12].copy_from_slice(&[0; 8]);
    })
    .expect("no GPE0 block");
    assert_eq!(no_gpe.gpe0, Block::NONE);
    let command = |b: u8| core::num::NonZeroU8::new(b).expect("a command");
    let named = SmiCmd { port: 0xb2, acpi_enable: 2, acpi_disable: 3, s4bios_req: 0, pstate_cnt: 0, cst_cnt: 0 };
    assert_eq!(fixed(|_| {}).map(|f| f.smi_cmd), Ok(Some(named)));
    assert_eq!(named.legacy(), Some(LegacyMode { acpi_enable: command(2), acpi_disable: command(3) }));
    // No port is no `SMI_CMD`; a port with no way in, or a way in and no way
    // back, is one with no legacy mode to leave.
    assert_eq!(fixed(|t| t[48..52].fill(0)).map(|f| f.smi_cmd), Ok(None));
    for zeroed in [52, 53] {
        let smi_cmd = fixed(|t| t[zeroed] = 0).expect("a FADT").smi_cmd.expect("a port");
        assert_eq!((smi_cmd.port, smi_cmd.legacy()), (0xb2, None), "{zeroed}");
    }
    let method = fixed(|t| t[112] = 1 << 4).expect("a control-method button");
    assert_eq!(method.power_button, PowerButton::ControlMethod);
}

/// Every value Table 5.9 gives a meaning written to `SMI_CMD`, each from its
/// own byte, and the bytes either side of them read as none: `ACPI_ENABLE`
/// at 52, `ACPI_DISABLE` at 53, `S4BIOS_REQ` at 54, `PSTATE_CNT` at 55 and
/// `CST_CNT` at 95. A zero names no value in four of them, as the table says
/// of each; `S4BIOS_REQ` names its byte, zero too, exactly where the FACS's
/// `S4BIOS_F` is set.
#[test]
fn the_values_the_fadt_names_for_smi_cmd_are_each_read_from_their_own_byte() {
    let all = fixed(|t| {
        t[51..57].copy_from_slice(&[0, 0xa0, 0xa1, 0xa2, 0xa3, 0]);
        t[94..97].copy_from_slice(&[0x77, 0xa4, 0x77]);
    })
    .expect("a FADT naming all five")
    .smi_cmd
    .expect("a port");
    assert_eq!(all, SmiCmd { port: 0xb2, acpi_enable: 0xa0, acpi_disable: 0xa1, s4bios_req: 0xa2, pstate_cnt: 0xa3, cst_cnt: 0xa4 });
    assert_eq!(all.named(true), [Some(0xa0), Some(0xa1), Some(0xa2), Some(0xa3), Some(0xa4)]);
    assert_eq!(all.named(false), [Some(0xa0), Some(0xa1), None, Some(0xa3), Some(0xa4)]);
    for (at, named) in [(52, 0), (53, 1), (54, 2), (55, 3), (95, 4)] {
        let one = fixed(|t| {
            t[52..56].fill(0);
            t[95] = 0;
            t[at] = 0x5a;
        })
        .expect("a FADT naming one")
        .smi_cmd
        .expect("a port");
        let mut want = [None; 5];
        want[named] = Some(0x5a);
        // With the flag set `S4BIOS_REQ` names the byte it holds, a zero where it holds one.
        let mut flagged = want;
        flagged[2] = Some(if named == 2 { 0x5a } else { 0 });
        assert_eq!(one.named(true), flagged, "the byte at {at}, S4BIOS_F set");
        if named == 2 {
            want[2] = None;
        }
        assert_eq!(one.named(false), want, "the byte at {at}, S4BIOS_F clear");
    }
}

/// A FADT before revision 2 has no `X_` fields, so the bytes past 116 are not
/// read even where the table runs on.
#[test]
fn a_revision_1_fadt_is_read_at_its_32_bit_fields_alone() {
    let mut body = vec![0u8; 240];
    body[56 - 36..60 - 36].copy_from_slice(&0x600u32.to_le_bytes());
    body[64 - 36..68 - 36].copy_from_slice(&0x604u32.to_le_bytes());
    body[88 - 36..90 - 36].copy_from_slice(&[4, 2]);
    // An `X_PM1a_EVT_BLK` that would disagree, were it read.
    body[148 - 36] = 1;
    body[152 - 36] = 0x99;
    let t = sdt(b"FACP", 1, &body);
    let regions: &[(u64, &[u8])] = &[(TABLE_AT, &t)];
    let fadt = Table::open(Machine { regions }, TABLE_AT, b"FACP", FADT_FOR_FIXED_HARDWARE).expect("FADT");
    assert_eq!(fixed_hardware(&fadt).map(|f| f.pm1a_event), Ok(Block { port: 0x600, len: 4 }));
}

fn ec_table(edit: impl FnOnce(&mut [u8])) -> Result<toyos_acpi::Ec, EcRefused> {
    let mut body = vec![0u8; ECDT_NEEDED - 36];
    body[..12].copy_from_slice(&[1, 8, 0, 0, 0x66, 0, 0, 0, 0, 0, 0, 0]);
    body[12..24].copy_from_slice(&[1, 8, 0, 0, 0x62, 0, 0, 0, 0, 0, 0, 0]);
    body[28] = 0x6e;
    edit(&mut body);
    let t = sdt(b"ECDT", 1, &body);
    let regions: &[(u64, &[u8])] = &[(TABLE_AT, &t)];
    ecdt(&Table::open(Machine { regions }, TABLE_AT, b"ECDT", 36).expect("ECDT"))
}

#[test]
fn an_ecdt_naming_no_port_controller_is_refused_by_name() {
    assert_eq!(ec_table(|_| {}), Ok(toyos_acpi::Ec { command: 0x66, data: 0x62, gpe: 0x6e }));
    assert_eq!(
        ec_table(|b| b[0] = 0),
        Err(EcRefused::NotSystemIo { register: Register::Command, space: 0 }),
        "a controller in memory"
    );
    assert_eq!(
        ec_table(|b| b[13] = 16),
        Err(EcRefused::Width { register: Register::Data, bit_width: 16, bit_offset: 0 })
    );
    assert_eq!(
        ec_table(|b| b[4] = 0),
        Err(EcRefused::Address { register: Register::Command, address: 0 }),
        "an ECDT of zeros, which some firmware publishes"
    );
    assert_eq!(
        ec_table(|b| b[16 + 2] = 1),
        Err(EcRefused::Address { register: Register::Data, address: 0x1_0062 })
    );
    let short = sdt(b"ECDT", 1, &[1, 8, 0, 0, 0x66, 0, 0, 0, 0, 0, 0, 0]);
    let regions: &[(u64, &[u8])] = &[(TABLE_AT, &short)];
    let table = Table::open(Machine { regions }, TABLE_AT, b"ECDT", 36).expect("ECDT");
    assert_eq!(ecdt(&table), Err(EcRefused::Short { len: 48 }));
}

/// Table 5.9: the OS treats the SCI as level and active low. Where no override
/// names it, and where one names it conforming, that is what it is; where one
/// says otherwise, what it says.
#[test]
fn the_sci_defaults_to_level_and_active_low_where_nothing_says_otherwise() {
    let level_low = |gsi| Line { gsi, trigger: Trigger::Level, polarity: Polarity::Low };
    assert_eq!(sci_line(9, &[]), level_low(9), "no override");
    let conforms = SourceOverride { bus: 0, source_irq: 9, gsi: 20, flags: 0 };
    assert_eq!(sci_line(9, &[conforms]), level_low(20), "an override that conforms");
    let edge_high = SourceOverride { flags: 0b0101, ..conforms };
    assert_eq!(sci_line(9, &[edge_high]), Line { gsi: 20, trigger: Trigger::Edge, polarity: Polarity::High });
    let polarity_only = SourceOverride { flags: 0b0001, ..conforms };
    assert_eq!(sci_line(9, &[polarity_only]), Line { gsi: 20, trigger: Trigger::Level, polarity: Polarity::High });
    // An ISA line that conforms keeps the ISA bus's edge, active high.
    assert_eq!(isa_line(9, &[conforms]), Line { gsi: 20, trigger: Trigger::Edge, polarity: Polarity::High });
    assert_eq!(isa_line(1, &[conforms]), Line { gsi: 1, trigger: Trigger::Edge, polarity: Polarity::High });
}
