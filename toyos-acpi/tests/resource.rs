//! The resource-descriptor decoder, over lists laid out in a machine's memory
//! the way firmware leaves them.

mod common;

use common::{qword_descriptor, resource_list as list, Machine, BUS, IO, MEMORY};
use toyos_abi::boot::RootBridgeWindow;
use toyos_acpi::{io_ports, memory_windows, ResourceError, MAX_LIST_BYTES};

/// Where a firmware pool allocation sits in the crafted machines below.
const AT: u64 = 0x7f00_1234;

/// A well-formed descriptor: no granularity, its maximum its minimum plus its
/// length.
fn qword(kind: u8, min: u64, length: u64, translation: u64) -> Vec<u8> {
    qword_descriptor(kind, 0, min, min + length - 1, translation, length)
}

/// One walk of `bytes`, in a machine holding `bytes` at [`AT`] and nothing
/// else — so a decoder reading one byte past the list panics rather than
/// answering.
fn windows(bytes: &[u8], room: usize) -> Result<Vec<RootBridgeWindow>, ResourceError> {
    let regions: &[(u64, &[u8])] = &[(AT, bytes)];
    let mut out = vec![RootBridgeWindow::default(); room];
    let count = memory_windows(Machine { regions }, AT, &mut out)?;
    out.truncate(count);
    Ok(out)
}

/// The shape the T14's firmware answers in, as Linux's own journal reports it:
/// two memory windows and the I/O and bus ranges beside them, of which only the
/// memory ones are windows a BAR could be placed in.
#[test]
fn only_the_memory_ranges_of_a_root_bridge_become_windows() {
    let bytes = list(&[
        qword(BUS, 0, 0x7a, 0),
        qword(IO, 0, 0x0cf8, 0),
        qword(MEMORY, 0x000a_0000, 0x0002_0000, 0),
        qword(MEMORY, 0xa080_0000, 0x1f80_0000, 0),
        qword(MEMORY, 0x40_0000_0000, 0x40_0000_0000, 0),
    ]);
    assert_eq!(
        windows(&bytes, 8).expect("a list this decoder reads"),
        [
            RootBridgeWindow { base: 0x000a_0000, length: 0x0002_0000 },
            RootBridgeWindow { base: 0xa080_0000, length: 0x1f80_0000 },
            RootBridgeWindow { base: 0x40_0000_0000, length: 0x40_0000_0000 },
        ]
    );
}

/// A window of no length decodes nothing and is not one; the descriptor is
/// still stepped over and what follows it is still read.
#[test]
fn a_window_of_no_length_is_not_carried() {
    let mut empty = qword(MEMORY, 0, 1, 0);
    empty[22..30].copy_from_slice(&0u64.to_le_bytes());
    empty[38..46].copy_from_slice(&0u64.to_le_bytes());
    let bytes = list(&[empty, qword(MEMORY, 0xa080_0000, 0x1f80_0000, 0)]);
    assert_eq!(
        windows(&bytes, 8).expect("an empty range is a range"),
        [RootBridgeWindow { base: 0xa080_0000, length: 0x1f80_0000 }]
    );
}

/// The two accounts a descriptor carries of its own extent have to agree. This
/// is what a decoder reading the minimum out of the maximum's offset — or a
/// length out of the wrong one — fails on, on real firmware bytes as well as
/// here.
#[test]
fn a_maximum_that_is_not_the_minimum_plus_the_length_is_refused() {
    let mut d = qword(MEMORY, 0xa080_0000, 0x1f80_0000, 0);
    d[22..30].copy_from_slice(&0xbfff_fffeu64.to_le_bytes());
    assert_eq!(
        windows(&list(&[d]), 8),
        Err(ResourceError::Inconsistent {
            min: 0xa080_0000,
            max: 0xbfff_fffe,
            length: 0x1f80_0000
        })
    );

    // And the same descriptor with its minimum and maximum swapped, which is
    // the one-field mutation this decoder could plausibly carry.
    let mut swapped = qword(MEMORY, 0xa080_0000, 0x1f80_0000, 0);
    let (min, max) = (swapped[14..22].to_vec(), swapped[22..30].to_vec());
    swapped[14..22].copy_from_slice(&max);
    swapped[22..30].copy_from_slice(&min);
    assert!(matches!(windows(&list(&[swapped]), 8), Err(ResourceError::Inconsistent { .. })));
}

/// A window whose addresses differ on the two sides of the bridge. What this
/// answers is which addresses a CPU may issue, and no machine in reach
/// translates one — so it is refused rather than carried untranslated.
#[test]
fn a_translated_window_is_refused_rather_than_taken_at_its_minimum() {
    let bytes = list(&[qword(MEMORY, 0x1000_0000, 0x1000, 0x4000_0000)]);
    assert_eq!(
        windows(&bytes, 8),
        Err(ResourceError::Translated { min: 0x1000_0000, offset: 0x4000_0000 })
    );
}

/// A range the bridge consumes is not one anything behind it decodes, and a BAR
/// moved into one would be moved onto the bridge's own registers.
///
/// The other three bits of the same byte say how the range is decoded and
/// whether its ends are fixed, and none of them changes what the range is.
#[test]
fn a_range_the_bridge_consumes_is_not_a_window_it_forwards() {
    let mut consumed = qword(MEMORY, 0xc000_0000, 0x1000_0000, 0);
    consumed[4] |= 1;
    assert_eq!(
        windows(&list(&[consumed]), 8),
        Err(ResourceError::Consumed { min: 0xc000_0000 })
    );

    for flags in [0b0000_0010u8, 0b0000_0100, 0b0000_1000] {
        let mut d = qword(MEMORY, 0xc000_0000, 0x1000_0000, 0);
        d[4] |= flags;
        assert_eq!(
            windows(&list(&[d]), 8).expect("a forwarded range"),
            [RootBridgeWindow { base: 0xc000_0000, length: 0x1000_0000 }]
        );
    }
}

/// The narrower address space descriptors, and anything else: refused by the
/// tag rather than stepped over. A window silently dropped is an aperture the
/// kernel would then place a BAR outside of.
#[test]
fn a_descriptor_this_decoder_does_not_read_is_refused_by_its_tag() {
    for tag in [0x87u8, 0x88, 0x8B] {
        let mut d = vec![tag, 0x17, 0x00];
        d.resize(3 + 0x17, 0);
        assert_eq!(windows(&list(&[d]), 8), Err(ResourceError::UnknownTag { tag }));
    }
}

/// And the resource type inside a descriptor this decoder does read: reserved
/// or vendor-defined, and refused for the tag's reason — a range whose kind is
/// unread may be memory.
#[test]
fn a_resource_type_this_decoder_does_not_read_is_refused_by_its_kind() {
    for kind in [3u8, 191, 192, 255] {
        let bytes = list(&[qword(kind, 0xa080_0000, 0x1000, 0)]);
        assert_eq!(windows(&bytes, 8), Err(ResourceError::UnknownResourceType { kind }));
    }
}

/// A QWORD descriptor too small to hold the fields its tag defines is refused,
/// and the refusal names the whole of it against what those fields need.
#[test]
fn a_short_descriptor_is_refused_rather_than_read_past() {
    let mut d = qword(MEMORY, 0xa080_0000, 0x1000, 0);
    d[1] = 0x2A;
    d.truncate(45);
    assert_eq!(
        windows(&list(&[d]), 8),
        Err(ResourceError::Short { tag: 0x8A, whole: 45, needed: 46 })
    );
}

/// More windows than the caller has room for is a refusal and never a prefix:
/// a caller handed the windows decoded before the refusal would be holding an
/// aperture with a hole in it.
#[test]
fn more_windows_than_there_is_room_for_is_refused_whole() {
    let bytes = list(&[
        qword(MEMORY, 0x1000_0000, 0x1000, 0),
        qword(MEMORY, 0x2000_0000, 0x1000, 0),
        qword(MEMORY, 0x3000_0000, 0x1000, 0),
    ]);
    assert_eq!(windows(&bytes, 3).expect("room for three").len(), 3);
    assert_eq!(windows(&bytes, 2), Err(ResourceError::TooMany { room: 2 }));
}

/// A list with no End Tag ends the walk at the bound rather than running off
/// into whatever follows it.
#[test]
fn a_list_with_no_end_tag_stops_at_the_bound() {
    let mut bytes = Vec::new();
    while bytes.len() < MAX_LIST_BYTES + 46 {
        bytes.extend_from_slice(&qword(IO, 0x1000, 0x10, 0));
    }
    assert_eq!(windows(&bytes, 8), Err(ResourceError::Unterminated));
}

/// A list the reader runs out of is a refusal naming the bytes it wanted.
#[test]
fn a_list_that_runs_off_the_end_of_what_can_be_read_is_refused() {
    let mut bytes = qword(MEMORY, 0xa080_0000, 0x1000, 0);
    bytes.truncate(40);
    assert_eq!(windows(&bytes, 8), Err(ResourceError::Unreadable { at: AT, len: 46 }));
}

/// An I/O Port Descriptor (ACPI 6.5 §6.4.2.5): 16-bit decode, a run of `len`
/// ports whose base is anywhere from `min` to `max`.
fn io(min: u16, max: u16, len: u8) -> Vec<u8> {
    let [min_lo, min_hi] = min.to_le_bytes();
    let [max_lo, max_hi] = max.to_le_bytes();
    vec![0x47, 0x01, min_lo, min_hi, max_lo, max_hi, 0x00, len]
}

/// A Fixed Location I/O Port Descriptor (§6.4.2.6).
fn fixed_io(base: u16, len: u8) -> Vec<u8> {
    let [lo, hi] = base.to_le_bytes();
    vec![0x4B, lo, hi, len]
}

/// The first port of every run `bytes` names, read from [`AT`].
fn ports(bytes: &[u8], room: usize) -> Result<Vec<u16>, ResourceError> {
    let regions: &[(u64, &[u8])] = &[(AT, bytes)];
    let mut out = vec![0; room];
    let count = io_ports(Machine { regions }, AT, &mut out)?;
    out.truncate(count);
    Ok(out)
}

/// An AMD laptop's embedded controller, as its `_CRS` names it, decoded:
/// two I/O Port Descriptors of one port each, 0x62 then 0x66, each fixed by
/// a minimum equal to its maximum. ACPI 6.5 §12.11 puts the data register
/// first and the command/status register second; the order is the list's.
#[test]
fn an_amd_laptops_controller_is_two_fixed_ports_in_the_lists_order() {
    let crs = list(&[io(0x62, 0x62, 1), io(0x66, 0x66, 1)]);
    assert_eq!(ports(&crs, 2), Ok(vec![0x62, 0x66]));
    // The same list as an evaluation hands the buffer over, from address 0.
    let mut out = [0; 2];
    assert_eq!(io_ports(&crs[..], 0, &mut out), Ok(2));
    assert_eq!(out, [0x62, 0x66]);
    // The fixed form names the same two, its base in ten bits.
    assert_eq!(ports(&list(&[fixed_io(0x62, 1), fixed_io(0xFC66, 1)]), 2), Ok(vec![0x62, 0x66]));
}

/// A run the OS would choose within, a run of no ports, any other descriptor
/// and more runs than there is room for are each refused by name, never
/// stepped over.
#[test]
fn an_io_run_that_is_no_fixed_port_is_refused_by_name() {
    assert_eq!(ports(&list(&[io(0x62, 0x6A, 1)]), 2), Err(ResourceError::Relocatable { min: 0x62, max: 0x6A }));
    assert_eq!(ports(&list(&[io(0x62, 0x62, 0)]), 2), Err(ResourceError::NoPorts { port: 0x62 }));
    assert_eq!(ports(&list(&[fixed_io(0x66, 0)]), 2), Err(ResourceError::NoPorts { port: 0x66 }));
    // IRQ (§6.4.2.1) beside the two ports.
    assert_eq!(ports(&list(&[io(0x62, 0x62, 1), vec![0x22, 0x01, 0x00]]), 2), Err(ResourceError::UnknownTag { tag: 0x22 }));
    let three = list(&[io(0x62, 0x62, 1), io(0x66, 0x66, 1), io(0x68, 0x68, 1)]);
    assert_eq!(ports(&three, 2), Err(ResourceError::TooMany { room: 2 }));
    assert_eq!(ports(&[0x47, 0x01, 0x62], 2), Err(ResourceError::Unreadable { at: AT, len: 8 }));
    let unterminated = io(0x62, 0x62, 1).repeat(MAX_LIST_BYTES / 8 + 1);
    assert_eq!(ports(&unterminated, MAX_LIST_BYTES), Err(ResourceError::Unterminated));
}
