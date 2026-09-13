//! The resource-descriptor decoder, over lists laid out in a machine's memory
//! the way firmware leaves them.

mod common;

use common::Machine;
use toyos_abi::boot::RootBridgeWindow;
use toyos_acpi::{list_len, memory_windows, ResourceError, MAX_LIST_BYTES};

/// Where a firmware pool allocation sits in the crafted machines below.
const AT: u64 = 0x7f00_1234;

/// ACPI 6.5 Table 6.44's resource types.
const MEMORY: u8 = 0;
const IO: u8 = 1;
const BUS: u8 = 2;

/// One QWORD Address Space Descriptor (ACPI 6.5 §6.4.3.5.1), well formed.
fn qword(kind: u8, min: u64, length: u64, translation: u64) -> Vec<u8> {
    let max = min + length - 1;
    let mut d = vec![0x8A, 0x2B, 0x00, kind, 0x00, 0x00];
    d.extend_from_slice(&0u64.to_le_bytes());
    d.extend_from_slice(&min.to_le_bytes());
    d.extend_from_slice(&max.to_le_bytes());
    d.extend_from_slice(&translation.to_le_bytes());
    d.extend_from_slice(&length.to_le_bytes());
    d
}

/// ACPI 6.5 §6.4.2.9, over its checksum byte.
fn end_tag() -> Vec<u8> {
    vec![0x79, 0x00]
}

fn list(descriptors: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for d in descriptors {
        bytes.extend_from_slice(d);
    }
    bytes.extend_from_slice(&end_tag());
    bytes
}

/// A machine holding `bytes` at [`AT`] and nothing else, so a decoder reading
/// one byte past the list panics rather than answering.
fn machine(bytes: &[u8]) -> Machine<'_> {
    Machine { regions: core::slice::from_ref(Box::leak(Box::new((AT, bytes)))) }
}

fn windows(bytes: &[u8], room: usize) -> Result<Vec<RootBridgeWindow>, ResourceError> {
    let mut out = vec![RootBridgeWindow::default(); room];
    let count = memory_windows(machine(bytes), AT, &mut out)?;
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
    assert_eq!(list_len(machine(&bytes), AT), bytes.len());
    assert_eq!(
        windows(&bytes, 8).expect("a list firmware could emit"),
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

/// A QWORD descriptor whose own declared length cannot hold the fields its tag
/// defines is refused, and its declared length is what the refusal names.
#[test]
fn a_short_descriptor_is_refused_rather_than_read_past() {
    let mut d = qword(MEMORY, 0xa080_0000, 0x1000, 0);
    d[1] = 0x2A;
    d.truncate(45);
    assert_eq!(
        windows(&list(&[d]), 8),
        Err(ResourceError::Short { tag: 0x8A, declared: 45, needed: 46 })
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
    assert_eq!(list_len(machine(&bytes), AT), MAX_LIST_BYTES);
}

/// A list the reader runs out of is a refusal naming the bytes it wanted, and
/// [`list_len`] answers with what it did reach — which is what the bootloader
/// logs when the decode below it refuses.
#[test]
fn a_list_that_runs_off_the_end_of_what_can_be_read_is_refused() {
    let mut bytes = qword(MEMORY, 0xa080_0000, 0x1000, 0);
    bytes.truncate(40);
    assert_eq!(
        windows(&bytes, 8),
        Err(ResourceError::Unreadable { at: AT, len: 46 })
    );
    assert_eq!(list_len(machine(&bytes), AT), 0);
}
