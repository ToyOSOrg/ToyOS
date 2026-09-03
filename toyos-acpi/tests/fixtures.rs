//! The differential: QEMU's own tables, decoded here, against what the kernel
//! printed on the boot they were taken off.

mod common;

use common::Machine;
use toyos_acpi::{
    century_of, dsdt_address, ecam_base, find_table, hpet_base, iapc_boot_arch, madt_entries,
    rtc_century, Century, IoApicEntry, MadtEntry, SourceOverride, TableError, FADT_PM1A_CNT_BLK,
    MADT_ENTRIES,
};

/// Where each table sat in that guest's physical memory. The XSDT's entries
/// point at these addresses, so the walk under test is the real one.
const RSDP: u64 = 0x7fb7_e014;
const REGIONS: &[(u64, &[u8])] = &[
    (RSDP, include_bytes!("../fixtures/rsdp.bin")),
    (0x7fb7_d0e8, include_bytes!("../fixtures/xsdt.bin")),
    (0x7fb7_9000, include_bytes!("../fixtures/facp.bin")),
    (0x7fb7_8000, include_bytes!("../fixtures/apic.bin")),
    (0x7fb7_7000, include_bytes!("../fixtures/hpet.bin")),
    (0x7fb7_6000, include_bytes!("../fixtures/mcfg.bin")),
    (0x7fb7_5000, include_bytes!("../fixtures/dmar.bin")),
    (0x7fb7_4000, include_bytes!("../fixtures/waet.bin")),
];

fn machine() -> Machine<'static> {
    Machine { regions: REGIONS }
}

/// `ACPI: MADT cpus=[0, 1]`, and the I/O APIC and override lines the driver
/// printed out of the same walk.
#[test]
fn the_madt_names_the_two_cpus_that_boot_and_the_chip_that_interrupts_them() {
    let m = machine();
    let madt = find_table(m, RSDP, b"APIC", MADT_ENTRIES).expect("MADT");
    let mut cpus = Vec::new();
    let mut io_apics = Vec::new();
    let mut overrides = Vec::new();
    for item in madt_entries(&madt) {
        match item.expect("a table QEMU published walks to its end") {
            MadtEntry::LocalApic { apic_id, enabled } => {
                if enabled {
                    cpus.push(apic_id);
                }
            }
            MadtEntry::IoApic(e) => io_apics.push(e),
            MadtEntry::SourceOverride(o) => overrides.push(o),
            MadtEntry::Other(_) => {}
        }
    }

    assert_eq!(cpus, [0, 1], "ACPI: MADT cpus=[0, 1]");
    assert_eq!(
        io_apics,
        [IoApicEntry { id: 0, address: 0xfec0_0000, gsi_base: 0 }],
        "ioapic: id=0 at 0xfec00000"
    );
    // `ioapic: iso bus:irq->gsi [0:0->2 edge/high, 0:5->5 level/high,
    //  0:9->9 level/high, 0:10->10 level/high, 0:11->11 level/high]`.
    // MPS INTI: polarity bits 0-1 (1 = active high), trigger bits 2-3
    // (1 = edge, 3 = level) — ACPI 6.5 Table 5.24.
    assert_eq!(
        overrides,
        [
            SourceOverride { bus: 0, source_irq: 0, gsi: 2, flags: 0x0 },
            SourceOverride { bus: 0, source_irq: 5, gsi: 5, flags: 0xd },
            SourceOverride { bus: 0, source_irq: 9, gsi: 9, flags: 0xd },
            SourceOverride { bus: 0, source_irq: 10, gsi: 10, flags: 0xd },
            SourceOverride { bus: 0, source_irq: 11, gsi: 11, flags: 0xd },
        ]
    );
}

/// `ACPI: MCFG found at 0x7fb76000` and `ACPI: ECAM base address: 0xb0000000`.
#[test]
fn the_mcfg_names_the_ecam_window_the_pci_walk_used() {
    let (mcfg, base) = ecam_base(machine(), RSDP).expect("MCFG");
    assert_eq!(mcfg.base(), 0x7fb7_6000);
    assert_eq!(base, 0xb000_0000);
}

/// `ACPI: HPET at 0xfed00000`.
#[test]
fn the_hpet_table_names_the_window_the_timer_driver_mapped() {
    assert_eq!(hpet_base(machine(), RSDP), Ok(0xfed0_0000));
}

/// `ACPI: the FADT puts the RTC century register at CMOS 0x32`.
#[test]
fn the_fadt_names_the_century_register_the_wall_clock_read() {
    assert_eq!(rtc_century(machine(), RSDP), Ok(Century::At(0x32)));
    // The actuator's override is applied to the raw byte, not to the verdict.
    assert_eq!(century_of(0), Century::Absent);
}

/// `ACPI: PM1a=0x604`, and the DSDT this boot found `\_S5_` in.
#[test]
fn the_fadt_names_the_power_block_and_the_dsdt() {
    let m = machine();
    let fadt = find_table(m, RSDP, b"FACP", FADT_PM1A_CNT_BLK + 4).expect("FADT");
    assert_eq!(fadt.u32_at(FADT_PM1A_CNT_BLK), Some(0x604));
    assert_eq!(dsdt_address(&fadt), 0x7fb7_a000);
}

#[test]
fn the_boot_architecture_flags_come_off_a_revision_that_defines_them() {
    let (revision, flags) = iapc_boot_arch(machine(), RSDP).expect("FADT");
    assert_eq!((revision, flags), (3, 0x2));
}

#[test]
fn the_xsdt_walk_reaches_every_entry() {
    let m = machine();
    for (signature, at) in [
        (b"FACP", 0x7fb7_9000u64),
        (b"APIC", 0x7fb7_8000),
        (b"HPET", 0x7fb7_7000),
        (b"MCFG", 0x7fb7_6000),
        (b"DMAR", 0x7fb7_5000),
        (b"WAET", 0x7fb7_4000),
    ] {
        let table = find_table(m, RSDP, signature, 36)
            .unwrap_or_else(|e| panic!("{}: {e:?}", core::str::from_utf8(signature).unwrap()));
        assert_eq!(table.base(), at);
    }
    assert_eq!(find_table(m, RSDP, b"SSDT", 36).err(), Some(TableError::Absent));
}
