//! The T14's fixed hardware and SCI, decoded from an extract of its FACP and
//! APIC and held to what ACPI 6.5 says the machine's bytes mean.
//!
//! **An extract, never the tables**: each table here is laid out from the
//! fields `fixed_hardware`, `pm1a_control` and `sci_line` read, at their offsets and
//! with the machine's own bytes, and zeros everywhere else, then sealed. The
//! whole tables are checked against these fields outside the tree.

mod common;

use common::{entry, madt, sdt, Machine};
use toyos_abi::acpi::Block;
use toyos_acpi::{
    fixed_hardware, madt_entries, pm1a_control, sci_line, FixedHardware, Line, MadtEntry,
    Polarity, PowerButton, SmiCmd, SourceOverride, Table, Trigger, FADT_FOR_FIXED_HARDWARE,
    MADT_ENTRIES,
};

/// The FACP's fixed-hardware fields: (offset, the machine's bytes there).
/// Revision 6, 276 bytes long; a Generic Address Structure is space, bit
/// width, bit offset, access size, then the address. `X_GPE0_BLK` names a bit
/// width of 0, as this firmware writes it.
const FACP_LEN: usize = 276;
const FACP_REVISION: u8 = 6;
const FACP_FIELDS: &[(usize, &[u8])] = &[
    (46, &[0x09, 0x00]),
    (48, &[0xb2, 0x00, 0x00, 0x00]),
    (52, &[0xf0, 0xf1]),
    (56, &[0x00, 0x18, 0x00, 0x00]),
    (64, &[0x04, 0x18, 0x00, 0x00]),
    (80, &[0x60, 0x18, 0x00, 0x00]),
    (88, &[0x04, 0x02]),
    (92, &[0x20, 0x00]),
    (112, &[0xe5, 0xc4, 0x20, 0x00]),
    (148, &[0x01, 0x20, 0x00, 0x02, 0x00, 0x18, 0, 0, 0, 0, 0, 0]),
    (160, &[0x01, 0x00, 0x00, 0x02, 0, 0, 0, 0, 0, 0, 0, 0]),
    (172, &[0x01, 0x10, 0x00, 0x02, 0x04, 0x18, 0, 0, 0, 0, 0, 0]),
    (184, &[0x01, 0x00, 0x00, 0x02, 0, 0, 0, 0, 0, 0, 0, 0]),
    (220, &[0x01, 0x00, 0x00, 0x01, 0x60, 0x18, 0, 0, 0, 0, 0, 0]),
    (232, &[0x01, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0, 0, 0]),
];

/// The APIC's two interrupt source overrides, whole.
const APIC_OVERRIDES: &[[u8; 10]] = &[
    [0x02, 0x0a, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x02, 0x0a, 0x00, 0x09, 0x09, 0x00, 0x00, 0x00, 0x0d, 0x00],
];

const AT: u64 = 0x9000_0000;

fn laid_out(signature: &[u8; 4], revision: u8, len: usize, fields: &[(usize, &[u8])]) -> Vec<u8> {
    let mut body = vec![0u8; len - 36];
    for &(at, bytes) in fields {
        body[at - 36..at - 36 + bytes.len()].copy_from_slice(bytes);
    }
    sdt(signature, revision, &body)
}

/// `bytes` at [`AT`], opened; leaked, so the reader a table carries outlives
/// the test.
fn open(bytes: Vec<u8>, signature: &[u8; 4], needed: usize) -> Table<Machine<'static>> {
    let bytes: &'static [u8] = Box::leak(bytes.into_boxed_slice());
    let regions: &'static [(u64, &[u8])] = Box::leak(Box::new([(AT, bytes)]));
    Table::open(Machine { regions }, AT, signature, needed).expect("a sealed extract")
}

/// ACPI 6.5 Table 5.9 at the machine's offsets: `SCI_INT` 9; `SMI_CMD` 0xb2,
/// `ACPI_ENABLE` 0xf0, `ACPI_DISABLE` 0xf1; `PM1a_EVT_BLK` 0x1800 of
/// `PM1_EVT_LEN` 4, `PM1a_CNT_BLK` 0x1804 of `PM1_CNT_LEN` 2, `GPE0_BLK` 0x1860
/// of `GPE0_BLK_LEN` 32; and Table 5.10's `PWR_BUTTON` clear, the
/// fixed-feature power button.
#[test]
fn the_t14s_fadt_names_its_fixed_hardware_and_sci() {
    let bytes = laid_out(b"FACP", FACP_REVISION, FACP_LEN, FACP_FIELDS);
    let fadt = open(bytes, b"FACP", FADT_FOR_FIXED_HARDWARE);
    assert_eq!(
        fixed_hardware(&fadt),
        Ok(FixedHardware {
            sci_int: 9,
            smi_cmd: Some(SmiCmd { port: 0xb2, acpi_enable: 0xf0, acpi_disable: 0xf1, s4bios_req: 0, pstate_cnt: 0, cst_cnt: 0 }),
            pm1a_event: Block { port: 0x1800, len: 4 },
            gpe0: Block { port: 0x1860, len: 32 },
            power_button: PowerButton::Fixed,
        })
    );
    assert_eq!(pm1a_control(&fadt), Ok(Block { port: 0x1804, len: 2 }));
}

/// ACPI 6.5 Tables 5.25 and 5.26 over the second override: bus 0, source 9,
/// global system interrupt 9, flags 0x000d, polarity 01 active high and
/// trigger mode 11 level-triggered.
#[test]
fn the_t14s_madt_names_its_sci_level_and_active_high() {
    let list: Vec<u8> = APIC_OVERRIDES.iter().flat_map(|o| entry(o[0], o[1], &o[2..])).collect();
    let table = open(madt(&list), b"APIC", MADT_ENTRIES);
    let overrides: Vec<SourceOverride> = madt_entries(&table)
        .filter_map(|e| match e {
            Ok(MadtEntry::SourceOverride(o)) => Some(o),
            _ => None,
        })
        .collect();
    assert_eq!(sci_line(9, &overrides), Line { gsi: 9, trigger: Trigger::Level, polarity: Polarity::High });
}

