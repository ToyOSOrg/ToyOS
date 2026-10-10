//! The MCFG's allocations: the window each decodes, the address each of its
//! functions is at, and every structure that is not a window refused by name
//! while the walk goes on to the next.

mod common;

use common::{declare_len, rsdp, sdt, xsdt, Machine};
use toyos_abi::boot::MemoryMapEntry;
use toyos_acpi::{ecam_allocations, AllocationRefused, ConfigRegister, EcamWindow};

const RSDP_AT: u64 = 0x1_0000;
const XSDT_AT: u64 = 0x2_0000;
const MCFG_AT: u64 = 0x3_0000;
const MIB: u64 = 1 << 20;

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
fn allocations(table: &[u8]) -> Vec<Result<EcamWindow, AllocationRefused>> {
    let head = rsdp(XSDT_AT, 2, 36);
    let root = xsdt(&[MCFG_AT]);
    let regions: &[(u64, &[u8])] = &[(RSDP_AT, &head), (XSDT_AT, &root), (MCFG_AT, table)];
    ecam_allocations(Machine { regions }, RSDP_AT).expect("an MCFG with one structure at least").collect()
}

/// A window as `(base, first bus, last bus)`.
type Window = (u64, u8, u8);

fn shape(window: &EcamWindow) -> Window {
    (window.base(), *window.buses().start(), *window.buses().end())
}

/// [`allocations`], each accepted one as a [`Window`].
fn walk(table: &[u8]) -> Vec<Result<Window, AllocationRefused>> {
    allocations(table).iter().map(|item| item.as_ref().map(shape).map_err(|why| *why)).collect()
}

/// The one window an MCFG of one structure decodes.
fn window(base: u64, start_bus: u8, end_bus: u8) -> EcamWindow {
    allocations(&mcfg(&[(base, 0, start_bus, end_bus)]))[0].expect("a well-formed window")
}

/// The laptop's shape: one allocation of buses 0 to 0x3f. Its window is
/// 64 MiB, and the 192 MiB past it that 256 buses would name is not in it.
#[test]
fn a_window_of_sixty_four_buses_decodes_sixty_four_megabytes() {
    let accepted = window(0xf800_0000, 0, 0x3f);
    assert_eq!(accepted.decoded(), (0xf800_0000, 64 * MIB));
    assert!(accepted.holds(0x3f));
    assert!(!accepted.holds(0x40));
    assert_eq!(accepted.offset(0x3f, 31, 7), Some(64 * MIB - 0x1000));
    assert_eq!(accepted.offset(0x40, 0, 0), None);
    assert_eq!(accepted.locate(0xf800_0000 + 64 * MIB), None, "the first byte past the end bus");
}

/// The base is bus 0's whatever bus the window starts at, so the window's
/// first byte is its start bus's: a function's offset into the window counts
/// from that bus, and no bus below it or past its end has one.
#[test]
fn a_window_starting_at_bus_0x10_counts_its_functions_from_bus_0x10() {
    let accepted = window(0xe000_0000, 0x10, 0x1f);
    assert_eq!(accepted.decoded(), (0xe100_0000, 16 * MIB));
    assert_eq!(accepted.offset(0x10, 0, 0), Some(0));
    assert_eq!(accepted.offset(0x11, 2, 3), Some(MIB | 2 << 15 | 3 << 12));
    assert_eq!(accepted.offset(0x1f, 31, 7), Some(16 * MIB - 0x1000), "the window's last function");
    for (bus, device, function) in [(0x0f, 0, 0), (0x20, 0, 0), (0, 0, 0), (0x10, 32, 0), (0x10, 0, 8)] {
        assert_eq!(accepted.offset(bus, device, function), None, "{bus:#x}:{device:#x}.{function}");
    }
    assert_eq!(accepted.locate(0xe100_0000), Some(ConfigRegister { bus: 0x10, device: 0, function: 0, offset: 0 }));
    assert_eq!(accepted.locate(0xe0ff_ffff), None, "bus 0x0f's last byte");
    assert_eq!(accepted.locate(0xe200_0000), None, "bus 0x20's first byte");
    // Each function's every register is where `offset` puts it, and nowhere
    // else does `locate` name it.
    let (start, _) = accepted.decoded();
    for bus in accepted.buses() {
        for (device, function) in (0..32).flat_map(|device| (0..8).map(move |function| (device, function))) {
            let at = start + accepted.offset(bus, device, function).expect("a function the window decodes");
            for offset in [0, 0x44, 0xfff] {
                assert_eq!(accepted.locate(at + u64::from(offset)), Some(ConfigRegister { bus, device, function, offset }));
            }
        }
    }
}

/// Each malformed structure is refused by name, and the walk goes on to the
/// next.
#[test]
fn a_malformed_structure_is_refused_and_the_next_is_still_read() {
    let found = walk(&mcfg(&[
        (0xe000_0000, 1, 0, 0xff),
        (0xe000_0000, 0, 0x20, 0x1f),
        (0xe008_0000, 0, 0, 0xff),
        (0xffff_ffff_fff0_0000, 0, 0, 1),
        (0xe000_0000, 0, 0, 0xff),
    ]));
    assert_eq!(
        found,
        [
            Err(AllocationRefused::OtherSegment { segment: 1 }),
            Err(AllocationRefused::Inverted { start_bus: 0x20, end_bus: 0x1f }),
            Err(AllocationRefused::Misaligned { base: 0xe008_0000 }),
            Err(AllocationRefused::Wraps { base: 0xffff_ffff_fff0_0000, end_bus: 1 }),
            Ok((0xe000_0000, 0, 0xff)),
        ]
    );
}

/// A window whose last byte is the address space's last byte ends inside it;
/// one bus further does not.
#[test]
fn a_window_ending_on_the_last_byte_is_accepted_and_one_bus_more_wraps() {
    let top = 0xffff_ffff_fff0_0000;
    let accepted = window(top, 0, 0);
    assert_eq!(accepted.decoded(), (top, MIB));
    assert_eq!(accepted.locate(u64::MAX), Some(ConfigRegister { bus: 0, device: 31, function: 7, offset: 0xfff }));
    let base = 0xffff_ffff_f000_0000;
    assert_eq!(walk(&mcfg(&[(base, 0, 0xff, 0xff)])), [Ok((base, 0xff, 0xff))]);
    assert_eq!(walk(&mcfg(&[(top, 0, 1, 1)])), [Err(AllocationRefused::Wraps { base: top, end_bus: 1 })]);
}

/// Segment group 0 is served wherever its windows sit in the table: one on
/// another group is refused, and so is one naming a bus a window already
/// decodes.
#[test]
fn another_segment_and_an_overlap_are_refused() {
    let found = walk(&mcfg(&[
        (0x1_0000_0000, 1, 0, 0xff),
        (0xe000_0000, 0, 0, 0x7f),
        (0xf000_0000, 0, 0x7f, 0x80),
        (0xe000_0000, 0, 0x80, 0xff),
    ]));
    assert_eq!(
        found,
        [
            Err(AllocationRefused::OtherSegment { segment: 1 }),
            Ok((0xe000_0000, 0, 0x7f)),
            Err(AllocationRefused::Overlaps { start_bus: 0x7f, end_bus: 0x80 }),
            Ok((0xe000_0000, 0x80, 0xff)),
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
    assert_eq!(walk(&table), [Ok((0xe000_0000, 0, 0xff)), Err(AllocationRefused::Partial { bytes: 9 })]);
}

const fn e(uefi_type: u32, start: u64, end: u64) -> MemoryMapEntry {
    MemoryMapEntry { uefi_type, start, end }
}

const GRAIN_2M: u64 = 2 * MIB;
const GRAIN_4K: u64 = 0x1000;
const LIMIT: u64 = 1 << 47;

/// A window that starts or ends on an odd megabyte is off a 2 MiB grain,
/// where mapping it would take the neighbouring megabyte too; on a 4 KiB
/// grain it is mapped exactly.
#[test]
fn a_window_off_the_grain_is_refused_and_on_it_is_mapped() {
    let cases = [((0x11, 0x1f), Some((0xe110_0000, 15 * MIB))), ((0x10, 0x10), Some((0xe100_0000, MIB))), ((0x10, 0x1f), None)];
    for ((start_bus, end_bus), off) in cases {
        let accepted = window(0xe000_0000, start_bus, end_bus);
        let found = accepted.mappable(GRAIN_2M, LIMIT, &[]);
        match off {
            Some((start, bytes)) => assert_eq!(found, Err(AllocationRefused::OffGrain { start, bytes, grain: GRAIN_2M })),
            None => assert_eq!(found, Ok(())),
        }
        assert_eq!(accepted.mappable(GRAIN_4K, LIMIT, &[]), Ok(()));
    }
    // A base on an odd megabyte: aligned to a bus, not to the grain.
    assert_eq!(
        window(0xe010_0000, 0, 1).mappable(GRAIN_2M, LIMIT, &[]),
        Err(AllocationRefused::OffGrain { start: 0xe010_0000, bytes: 2 * MIB, grain: GRAIN_2M })
    );
}

/// The window's last byte must be below the first address the kernel cannot
/// map.
#[test]
fn a_window_reaching_the_limit_is_refused() {
    let below = window(LIMIT - 16 * MIB, 0, 0x0f);
    assert_eq!(below.mappable(GRAIN_2M, LIMIT, &[]), Ok(()));
    let reaching = window(LIMIT - 16 * MIB, 0, 0x11);
    assert_eq!(reaching.mappable(GRAIN_2M, LIMIT, &[]), Err(AllocationRefused::PastLimit { last: LIMIT + 2 * MIB - 1, limit: LIMIT }));
    let top = window(0xffff_ffff_fff0_0000, 0, 0);
    assert_eq!(top.mappable(GRAIN_4K, LIMIT, &[]), Err(AllocationRefused::PastLimit { last: u64::MAX, limit: LIMIT }));
}

/// Over memory firmware's map lists as memory — what the kernel hands out and
/// what ACPI's tables live in — a window is refused; over reserved or
/// memory-mapped I/O ranges, or beside memory, it is mapped.
#[test]
fn a_window_over_memory_is_refused() {
    let accepted = window(0x3f00_0000, 0, 0x0f);
    let beside = [e(7, 0x4000_0000, 0x8000_0000), e(7, 0x3000_0000, 0x3f00_0000), e(0, 0x3f00_0000, 0x4000_0000), e(11, 0x3f00_0000, 0x3f10_0000)];
    assert_eq!(accepted.mappable(GRAIN_4K, LIMIT, &beside), Ok(()));
    for (uefi_type, start, end) in [(7, 0x3fff_f000, 0x4000_1000), (7, 0x3000_0000, 0x3f00_1000), (4, 0x3f80_0000, 0x3f80_1000), (9, 0x3f00_0000, 0x3f00_1000), (10, 0x3ff0_0000, 0x4000_0000)] {
        let map = [e(0, 0x3f00_0000, 0x4000_0000), e(uefi_type, start, end)];
        assert_eq!(accepted.mappable(GRAIN_4K, LIMIT, &map), Err(AllocationRefused::OverMemory { uefi_type, start }), "{uefi_type} at {start:#x}");
    }
}
