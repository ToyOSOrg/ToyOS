//! The reader a kernel decodes firmware's tables through: bounded by the
//! direct map it reads them in, not by the architecture's physical-address
//! width.

mod common;

use common::{rsdp, sdt, xsdt, Machine};
use toyos_acpi::{find_table, Mapped, Phys, Table, TableError};
use toyos_bootmap::x86_64;

/// What edk2's map on QEMU q35 with 2 GiB gives the direct map: the boot
/// map's, as `toyos-bootmap`'s `the_reserved_hole_below_1_tib_is_not_mapped`
/// holds.
fn end() -> u64 {
    x86_64::direct_map_end(&[]).unwrap().get()
}

/// The reserved hole that edk2 describes below 1 TiB, inside 52 bits.
const HOLE: u64 = 0xfd_0000_0000;

const RSDP_AT: u64 = 0x1_0000;
const XSDT_AT: u64 = 0x2_0000;

fn mapped<'a>(regions: &'a [(u64, &'a [u8])]) -> Mapped<Machine<'a>> {
    Mapped::new(Machine { regions }, x86_64::direct_map_end(&[]).unwrap())
}

#[test]
fn a_table_past_the_direct_map_is_refused_before_a_byte_is_read() {
    let hpet = sdt(b"HPET", 1, &[0u8; 20]);
    let len = hpet.len();
    let end = end();
    for (at, refused) in [(HOLE, 36), (end, 36), (end - len as u64 + 1, len)] {
        let regions: &[(u64, &[u8])] = &[(at, &hpet)];
        assert_eq!(
            Table::open(mapped(regions), at, b"HPET", 0).err(),
            Some(TableError::Unmapped { at, len: refused }),
            "a table at {at:#x}"
        );
    }
    let at = end - len as u64;
    let regions: &[(u64, &[u8])] = &[(at, &hpet)];
    assert!(Table::open(mapped(regions), at, b"HPET", 0).is_ok(), "a table whose last byte is the map's");
}

/// The machine holds the table and the walk reads it; through the direct map
/// the entry is one it cannot reach, and is skipped.
#[test]
fn an_xsdt_entry_past_the_direct_map_is_skipped() {
    let head = rsdp(XSDT_AT, 2, 36);
    let root = xsdt(&[HOLE]);
    let hpet = sdt(b"HPET", 1, &[0u8; 20]);
    let regions: &[(u64, &[u8])] = &[(RSDP_AT, &head), (XSDT_AT, &root), (HOLE, &hpet)];
    assert!(find_table(Machine { regions }, RSDP_AT, b"HPET", 0).is_ok());
    assert_eq!(find_table(mapped(regions), RSDP_AT, b"HPET", 0).err(), Some(TableError::Absent));
}

#[test]
fn address_zero_and_a_range_past_every_address_are_refused() {
    let m = mapped(&[]);
    assert!(!m.readable(0, 1), "address zero");
    assert!(!m.readable(u64::MAX, 2), "a range whose end does not fit an address");
    assert!(m.readable(1, 1));
}
