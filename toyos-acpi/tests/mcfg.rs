//! The MCFG's allocations: the window each decodes, and every structure that
//! is not a window refused by name while the walk goes on to the next.

mod common;

use common::{declare_len, rsdp, sdt, xsdt, Machine};
use toyos_acpi::{ecam_allocations, Allocation, AllocationRefused};

const RSDP_AT: u64 = 0x1_0000;
const XSDT_AT: u64 = 0x2_0000;
const MCFG_AT: u64 = 0x3_0000;

/// An MCFG over `(base, segment, start bus, end bus)` structures.
fn mcfg(allocations: &[(u64, u16, u8, u8)]) -> Vec<u8> {
    let mut body = vec![0u8; 8];
    for (base, segment, start, end) in allocations {
        body.extend_from_slice(&base.to_le_bytes());
        body.extend_from_slice(&segment.to_le_bytes());
        body.extend_from_slice(&[*start, *end, 0, 0, 0, 0]);
    }
    sdt(b"MCFG", 1, &body)
}

/// Every item the walk answers over `table`.
fn allocations(table: &[u8]) -> Vec<Result<Allocation, AllocationRefused>> {
    let head = rsdp(XSDT_AT, 2, 36);
    let root = xsdt(&[MCFG_AT]);
    let regions: &[(u64, &[u8])] = &[(RSDP_AT, &head), (XSDT_AT, &root), (MCFG_AT, table)];
    ecam_allocations(Machine { regions }, RSDP_AT).expect("an MCFG with one structure at least").collect()
}

/// A window as `(base, segment, first bus, last bus)`.
type Window = (u64, u16, u8, u8);

fn window(allocation: &Allocation) -> Window {
    (allocation.base(), allocation.segment(), *allocation.buses().start(), *allocation.buses().end())
}

/// [`allocations`], each accepted one as a [`Window`].
fn walk(table: &[u8]) -> Vec<Result<Window, AllocationRefused>> {
    allocations(table).iter().map(|item| item.as_ref().map(window).map_err(|why| *why)).collect()
}

/// The laptop's shape: one allocation of buses 0 to 0x3f. Its window is
/// 64 MiB, and the 192 MiB past it that 256 buses would name is not in it.
#[test]
fn a_window_of_sixty_four_buses_decodes_sixty_four_megabytes() {
    let table = mcfg(&[(0xf800_0000, 0, 0, 0x3f)]);
    assert_eq!(walk(&table), [Ok((0xf800_0000, 0, 0, 0x3f))]);
    let accepted = allocations(&table)[0].unwrap();
    assert_eq!(accepted.decoded(), (0xf800_0000, 0x0400_0000));
    assert!(accepted.holds(0x3f));
    assert!(!accepted.holds(0x40));
}

/// The base is bus 0's whatever bus the window starts at, so the window's
/// first byte is its start bus's and no bus below it is held.
#[test]
fn a_window_starting_above_bus_zero_starts_at_its_start_bus() {
    let accepted = allocations(&mcfg(&[(0xe000_0000, 0, 0x10, 0x1f)]))[0].unwrap();
    assert_eq!(accepted.decoded(), (0xe100_0000, 0x0100_0000));
    assert!(!accepted.holds(0));
    assert!(!accepted.holds(0x0f));
    assert!(accepted.holds(0x10));
    assert!(!accepted.holds(0x20));
}

/// Each malformed structure is refused by name, and the walk goes on to the
/// next: a structure the walk refused names no served segment either.
#[test]
fn a_malformed_structure_is_refused_and_the_next_is_still_read() {
    let found = walk(&mcfg(&[
        (0xe000_0000, 1, 0x20, 0x1f),
        (0xe008_0000, 1, 0, 0xff),
        (0xffff_ffff_fff0_0000, 1, 0, 1),
        (0xe000_0000, 0, 0, 0xff),
    ]));
    assert_eq!(
        found,
        [
            Err(AllocationRefused::Inverted { start_bus: 0x20, end_bus: 0x1f }),
            Err(AllocationRefused::Misaligned { base: 0xe008_0000 }),
            Err(AllocationRefused::Wraps { base: 0xffff_ffff_fff0_0000, end_bus: 1 }),
            Ok((0xe000_0000, 0, 0, 0xff)),
        ]
    );
}

/// A window whose last byte is the address space's last byte ends inside it;
/// one bus further does not.
#[test]
fn a_window_ending_on_the_last_byte_is_accepted_and_one_bus_more_wraps() {
    let top = 0xffff_ffff_fff0_0000;
    let accepted = allocations(&mcfg(&[(top, 0, 0, 0)]))[0].unwrap();
    assert_eq!(accepted.decoded(), (top, 0x10_0000));
    let base = 0xffff_ffff_f000_0000;
    assert_eq!(walk(&mcfg(&[(base, 0, 0xff, 0xff)])), [Ok((base, 0, 0xff, 0xff))]);
    assert_eq!(walk(&mcfg(&[(top, 0, 1, 1)])), [Err(AllocationRefused::Wraps { base: top, end_bus: 1 })]);
}

/// The first window accepted names the served segment group; a structure on
/// another is refused, and so is one naming a bus a window already decodes.
#[test]
fn another_segment_and_an_overlap_are_refused() {
    let found = walk(&mcfg(&[
        (0xe000_0000, 0, 0, 0x7f),
        (0x1_0000_0000, 1, 0, 0xff),
        (0xf000_0000, 0, 0x7f, 0x80),
        (0xe000_0000, 0, 0x80, 0xff),
    ]));
    assert_eq!(
        found,
        [
            Ok((0xe000_0000, 0, 0, 0x7f)),
            Err(AllocationRefused::OtherSegment { segment: 1, served: 0 }),
            Err(AllocationRefused::Overlaps { start_bus: 0x7f, end_bus: 0x80 }),
            Ok((0xe000_0000, 0, 0x80, 0xff)),
        ]
    );
}

/// A table that ends inside a structure ends the walk on a refusal of that
/// structure, after every whole one.
#[test]
fn a_table_ending_inside_a_structure_refuses_the_part() {
    let mut table = mcfg(&[(0xe000_0000, 0, 0, 0xff), (0xf000_0000, 1, 0, 0xff)]);
    let declared = table.len() as u32 - 7;
    declare_len(&mut table, declared);
    assert_eq!(walk(&table), [Ok((0xe000_0000, 0, 0, 0xff)), Err(AllocationRefused::Partial { bytes: 9 })]);
}
