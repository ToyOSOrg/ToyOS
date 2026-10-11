//! The parser against tables somebody else wrote.
//!
//! Every test here starts from one valid image and breaks exactly one thing,
//! so a failure names the field. The point is not that the happy path works —
//! a QEMU boot covers that against a real firmware and a real disk — but that
//! none of the broken ones panics, allocates by a number the disk chose, or
//! returns a partition anyway.

mod table;

use table::{Image, Layout, RawEntry, Table};
use toyos_gpt::{GptError, Guid, Located, Stated};

const LBA: u32 = 512;
const ENTRY: u32 = 128;
const ARRAY_LBA: u64 = 2;
/// 128 entries of 128 bytes is 32 blocks of 512, which is why every GPT on
/// earth has its first usable block at 34.
const FIRST_USABLE: u64 = 34;
const DISK_LBAS: u64 = 2048;

const TYPE_ESP: Guid = Guid::EFI_SYSTEM;
const TYPE_OTHER: Guid = Guid([0x0F, 0xC6, 0x3D, 0xAF, 0x84, 0x83, 0x47, 0x72, 0x8E, 0x79, 0x3D, 0x69, 0xD8, 0x47, 0x7D, 0xE4]);

fn guid(n: u8) -> Guid {
    let mut b = [n; 16];
    b[0] = n;
    b[15] = n ^ 0xFF;
    Guid(b)
}

fn entry(index: u32, type_guid: Guid, unique: Guid, first: u64, last: u64) -> RawEntry {
    RawEntry { index, type_guid, unique, first, last, name: [0; 36] }
}

/// The disk every test here breaks one thing of, `edit`ed, with no backup
/// unless the edit [`mirrored`] one: a fallback must not paper over a primary
/// a test meant to break.
fn disk(edit: impl FnOnce(&mut Layout)) -> Image {
    let mut layout = Layout {
        lba_bytes: LBA,
        lba_count: DISK_LBAS,
        primary: Table {
            revision: 0x0001_0000,
            header_bytes: 92,
            reserved: 0,
            my_lba: 1,
            first_usable: FIRST_USABLE,
            last_usable: DISK_LBAS - FIRST_USABLE,
            disk_guid: guid(0x5D),
            entry_array_lba: ARRAY_LBA,
            entry_count: 128,
            entry_bytes: ENTRY,
            entries: vec![
                // Two ESP-typed decoys before the real one, and the real one
                // is neither first nor an obvious pick: a matcher that keys on
                // the type GUID, or takes the first used entry, or takes the
                // biggest, gets a different answer than the one asserted.
                entry(0, TYPE_ESP, guid(0xA1), 40, 99),
                entry(1, TYPE_OTHER, guid(0xB2), 100, 199),
                entry(2, TYPE_ESP, guid(0xC3), 200, 299),
                entry(3, TYPE_ESP, guid(0xD4), 300, 1999),
            ],
        },
        backup: None,
        reported_lba_count: DISK_LBAS,
        granularity: 1,
        mbr_type: 0xEE,
        mbr_signature: [0x55, 0xAA],
    };
    edit(&mut layout);
    layout.reported_lba_count = layout.lba_count;
    table::image(&layout)
}

/// A backup that mirrors the primary as it stands.
fn mirrored(layout: &mut Layout) {
    layout.backup = Some(layout.primary.mirror(layout.lba_bytes, layout.lba_count));
}

impl Image {
    fn at(&mut self, lba: u64, off: usize) -> &mut u8 {
        &mut self.bytes[lba as usize * self.lba_bytes as usize + off]
    }
    fn locate(&mut self, target: Guid) -> Result<Located, GptError> {
        toyos_gpt::locate(self, target)
    }
    fn locate_type(
        &mut self,
        target: Guid,
        out: &mut [Option<toyos_gpt::Entry>],
    ) -> Result<toyos_gpt::TypeScan, GptError> {
        toyos_gpt::locate_type(self, target, out)
    }
}

#[test]
fn finds_the_partition_by_unique_guid() {
    let mut img = disk(|_| {});
    let found = img.locate(guid(0xC3)).expect("the table has this GUID");
    assert_eq!(found.partition().index(), 2);
    assert_eq!(found.partition().first_lba(), 200);
    assert_eq!(found.partition().last_lba(), 299);
    assert_eq!(found.partition().lba_count().get(), 100);
    assert_eq!(found.used_entries(), 4);
    assert!(found.partition().is_efi_system());
    assert_eq!(found.disk_guid(), guid(0x5D));
}

/// The one that matters: three of the four entries are ESPs, so anything
/// selecting on the type GUID picks the wrong disk region. Each of the four
/// must come back as itself.
#[test]
fn each_guid_finds_its_own_entry() {
    let want = [
        (guid(0xA1), 0u32, 40u64, 99u64),
        (guid(0xB2), 1, 100, 199),
        (guid(0xC3), 2, 200, 299),
        (guid(0xD4), 3, 300, 1999),
    ];
    for (g, index, first, last) in want {
        let mut img = disk(|_| {});
        let found = img.locate(g).expect("present");
        assert_eq!((found.partition().index(), found.partition().first_lba(), found.partition().last_lba()), (index, first, last));
    }
}

/// The type scan's own case: the default table has three ESP-typed entries,
/// so a scan that stopped at the first, or answered with every used entry,
/// gets a different set than the one asserted.
#[test]
fn a_type_scan_lists_every_entry_of_that_type() {
    let mut img = disk(|_| {});
    let mut out = [None; 4];
    let scan = img.locate_type(TYPE_ESP, &mut out).expect("the table parses");
    assert_eq!((scan.matched, scan.used_entries), (3, 4));
    assert_eq!(scan.disk_guid, guid(0x5D));
    let found: Vec<Guid> = out.iter().flatten().flatten().map(|p| p.unique_guid()).collect();
    assert_eq!(found, vec![guid(0xA1), guid(0xC3), guid(0xD4)]);

    // A type nothing carries is not an error; it is an empty set.
    let none = img.locate_type(guid(0x77), &mut out).expect("the table parses");
    assert_eq!(none.matched, 0);
    assert!(out.iter().all(Option::is_none));
}

/// A slice too small does not truncate silently: the count of matches is the
/// table's, not the caller's, so the caller can tell it was not shown them all.
#[test]
fn a_type_scan_says_how_many_it_could_not_hand_back() {
    let mut img = disk(|_| {});
    let mut out = [None; 1];
    let scan = img.locate_type(TYPE_ESP, &mut out).expect("the table parses");
    assert_eq!(scan.matched, 3);
    assert_eq!(out[0].and_then(Result::ok).map(|p| p.unique_guid()), Some(guid(0xA1)));
}

/// The scan is held to the same CRC as the search: a damaged array yields no
/// candidates at all, rather than the ones read before the checksum failed.
#[test]
fn a_type_scan_over_a_damaged_array_is_refused() {
    let mut img = disk(|_| {});
    *img.at(ARRAY_LBA, 3) ^= 0x01;
    let mut out = [None; 4];
    assert!(matches!(
        img.locate_type(TYPE_ESP, &mut out),
        Err(GptError::EntryArrayCrc { .. })
    ));
}

#[test]
fn absent_guid_is_not_found_and_says_how_many_there_were() {
    let mut img = disk(|_| {});
    assert_eq!(img.locate(guid(0xEE)), Err(GptError::NotFound { used_entries: 4 }));
}

#[test]
fn a_zero_guid_matches_nothing() {
    let mut img = disk(|_| {});
    assert_eq!(img.locate(Guid::ZERO), Err(GptError::NotFound { used_entries: 4 }));
}

#[test]
fn no_protective_mbr() {
    let mut img = disk(|l| l.mbr_signature = [0, 0]);
    assert_eq!(img.locate(guid(0xC3)), Err(GptError::NoProtectiveMbr));

    let mut img = disk(|_| {});
    *img.at(0, 446 + 4) = 0x07;
    assert_eq!(img.locate(guid(0xC3)), Err(GptError::NoProtectiveMbr));
}

/// A protective record next to a real one means two tables describe this disk.
#[test]
fn hybrid_mbr_is_refused() {
    let mut img = disk(|_| {});
    *img.at(0, 446 + 16 + 4) = 0x83;
    assert_eq!(img.locate(guid(0xC3)), Err(GptError::NoProtectiveMbr));
}

#[test]
fn header_signature() {
    let mut img = disk(|_| {});
    *img.at(1, 0) = b'X';
    assert_eq!(img.locate(guid(0xC3)), Err(GptError::NoHeader));
}

#[test]
fn header_revision() {
    let mut img = disk(|l| l.primary.revision = 0x0002_0000);
    assert_eq!(img.locate(guid(0xC3)), Err(GptError::UnsupportedRevision(0x0002_0000)));
}

#[test]
fn header_size_bounds() {
    for bad in [0u32, 91, 513, u32::MAX] {
        let mut img = disk(|l| l.primary.header_bytes = bad);
        assert_eq!(img.locate(guid(0xC3)), Err(GptError::HeaderSize(bad)), "header_size {bad}");
    }
}

#[test]
fn header_reserved_word() {
    let mut img = disk(|l| l.primary.reserved = 1);
    assert_eq!(img.locate(guid(0xC3)), Err(GptError::HeaderReserved(1)));
}

#[test]
fn header_must_claim_lba_one() {
    let mut img = disk(|l| l.primary.my_lba = 2);
    assert_eq!(img.locate(guid(0xC3)), Err(GptError::HeaderMisplaced(2)));
}

#[test]
fn one_flipped_bit_in_the_header() {
    let mut img = disk(|_| {});
    *img.at(1, 80) ^= 0x01;
    match img.locate(guid(0xC3)) {
        Err(GptError::HeaderCrc { .. }) => {}
        other => panic!("a corrupt header parsed as {other:?}"),
    }
}

#[test]
fn one_flipped_bit_in_the_entry_array() {
    let mut img = disk(|_| {});
    *img.at(ARRAY_LBA, 32) ^= 0x01;
    match img.locate(guid(0xC3)) {
        Err(GptError::EntryArrayCrc { .. }) => {}
        other => panic!("a corrupt entry array parsed as {other:?}"),
    }
}

/// The CRC covers `entry_count * entry_bytes` bytes and not a byte more, so a
/// change past the end of the array must not be read as corruption — and must
/// not be read as an entry either.
#[test]
fn the_array_ends_where_the_header_says() {
    let mut img = disk(|l| l.primary.entry_count = 3);
    // Entry 3 is now outside the array. It is still on the disk.
    assert_eq!(img.locate(guid(0xD4)), Err(GptError::NotFound { used_entries: 3 }));
    *img.at(ARRAY_LBA, 3 * 128 + 1) ^= 0xFF;
    let found = img.locate(guid(0xC3)).expect("still parses");
    assert_eq!(found.used_entries(), 3);
}

#[test]
fn entry_size_must_be_a_power_of_two_multiple_of_128_that_fits_a_block() {
    for bad in [0u32, 1, 64, 127, 192, 1024, u32::MAX] {
        let mut img = disk(|l| l.primary.entry_bytes = bad);
        assert_eq!(img.locate(guid(0xC3)), Err(GptError::EntrySize(bad)), "entry size {bad}");
    }
}

#[test]
fn a_billion_entries_is_refused_not_read() {
    for bad in [u32::MAX, 1_000_000_000, 1025] {
        let mut img = disk(|l| l.primary.entry_count = bad);
        assert_eq!(
            img.locate(guid(0xC3)),
            Err(GptError::EntryArrayTooBig { entries: bad, entry_size: ENTRY }),
            "entry count {bad}"
        );
    }
}

/// 128 KiB exactly is the ceiling, and it is a ceiling on the array, not on
/// this disk: the array would have to fit before the first usable block too.
#[test]
fn the_array_ceiling_is_where_it_says_it_is() {
    let at_ceiling = (toyos_gpt::MAX_ENTRY_ARRAY_BYTES / ENTRY as u64) as u32;
    let mut over = disk(|l| l.primary.entry_count = at_ceiling + 1);
    assert!(matches!(over.locate(guid(0xC3)), Err(GptError::EntryArrayTooBig { .. })));

    let mut ok = disk(|l| {
        l.primary.entry_count = at_ceiling;
        l.primary.first_usable = 2 + toyos_gpt::MAX_ENTRY_ARRAY_BYTES / LBA as u64;
    });
    // Not TooBig: it is refused, if at all, for a different reason.
    assert!(!matches!(ok.locate(guid(0xC3)), Err(GptError::EntryArrayTooBig { .. })));
}

#[test]
fn zero_entries_is_refused() {
    let mut img = disk(|l| l.primary.entry_count = 0);
    assert_eq!(
        img.locate(guid(0xC3)),
        Err(GptError::EntryArrayTooBig { entries: 0, entry_size: ENTRY })
    );
}

#[test]
fn the_array_may_not_sit_on_the_header_or_past_the_usable_range() {
    for bad_lba in [0u64, 1] {
        let mut img = disk(|l| l.primary.entry_array_lba = bad_lba);
        assert!(
            matches!(img.locate(guid(0xC3)), Err(GptError::EntryArrayMisplaced { .. })),
            "array at LBA {bad_lba}"
        );
    }
    // Starts legally, ends past the first usable block.
    let mut img = disk(|l| l.primary.first_usable = 20);
    assert!(matches!(img.locate(guid(0xC3)), Err(GptError::EntryArrayMisplaced { .. })));
}

#[test]
fn an_array_lba_near_the_top_of_the_range_does_not_wrap() {
    let mut img = disk(|l| l.primary.entry_array_lba = u64::MAX - 1);
    assert!(matches!(img.locate(guid(0xC3)), Err(GptError::EntryArrayMisplaced { .. })));
}

#[test]
fn usable_range_must_be_a_range_inside_the_device() {
    let mut inverted = disk(|l| { l.primary.first_usable = 500; l.primary.last_usable = 100 });
    assert_eq!(
        inverted.locate(guid(0xC3)),
        Err(GptError::UsableRange { first: 500, last: 100 })
    );

    let mut past_end = disk(|l| l.primary.last_usable = DISK_LBAS);
    assert_eq!(
        past_end.locate(guid(0xC3)),
        Err(GptError::UsableRange { first: FIRST_USABLE, last: DISK_LBAS })
    );

    let mut over_the_table = disk(|l| l.primary.first_usable = 1);
    assert_eq!(
        over_the_table.locate(guid(0xC3)),
        Err(GptError::UsableRange { first: 1, last: DISK_LBAS - FIRST_USABLE })
    );
}

#[test]
fn a_partition_outside_the_disk_is_refused() {
    let mut img = disk(|l| l.primary.entries[2] = entry(2, TYPE_ESP, guid(0xC3), 200, u64::MAX));
    assert_eq!(
        img.locate(guid(0xC3)),
        Err(GptError::PartitionRange { first: 200, last: u64::MAX })
    );
}

#[test]
fn a_backwards_partition_is_refused() {
    let mut img = disk(|l| l.primary.entries[2] = entry(2, TYPE_ESP, guid(0xC3), 900, 800));
    assert_eq!(img.locate(guid(0xC3)), Err(GptError::PartitionRange { first: 900, last: 800 }));
}

/// A partition sitting on top of the entry array is the interesting shape:
/// the caller's next act is to write to it.
#[test]
fn a_partition_over_the_table_is_refused() {
    let mut img = disk(|l| l.primary.entries[2] = entry(2, TYPE_ESP, guid(0xC3), 3, 299));
    assert_eq!(img.locate(guid(0xC3)), Err(GptError::PartitionRange { first: 3, last: 299 }));
}

#[test]
fn an_overlapping_neighbour_is_refused() {
    let mut img = disk(|l| l.primary.entries[3] = entry(3, TYPE_OTHER, guid(0xD4), 250, 400));
    assert_eq!(img.locate(guid(0xC3)), Err(GptError::PartitionOverlap { index: 3 }));

    // And the overlap is found when it comes *before* the match too, which is
    // the case a single streaming pass would miss.
    let mut img = disk(|l| l.primary.entries[0] = entry(0, TYPE_OTHER, guid(0xA1), 40, 250));
    assert_eq!(img.locate(guid(0xC3)), Err(GptError::PartitionOverlap { index: 0 }));
}

/// UEFI puts the second copy at the end of the device precisely so a torn
/// write to the front is recoverable. A primary whose signature is gone
/// never became a checked table, so `locate` must retry the backup rather
/// than refuse a disk that is otherwise fine.
#[test]
fn a_damaged_primary_falls_back_to_a_good_backup() {
    let mut img = disk(mirrored);
    *img.at(1, 0) = b'X';
    let found = img.locate(guid(0xC3)).expect("the backup carries this GUID");
    assert_eq!(found.partition().index(), 2);
    assert_eq!(found.partition().first_lba(), 200);
    assert_eq!(found.partition().last_lba(), 299);
    assert_eq!(found.used_entries(), 4);
    assert_eq!(found.disk_guid(), guid(0x5D));
}

/// Both copies gone must be a named refusal, not a panic and not a made-up
/// answer. The fallback's own failure is discarded in favour of the
/// primary's, so the caller learns why the primary — the copy that matters —
/// was unreadable.
#[test]
fn both_copies_damaged_is_refused_by_name() {
    let mut img = disk(mirrored);
    *img.at(1, 0) = b'X';
    *img.at(DISK_LBAS - 1, 0) = b'X';
    assert_eq!(img.locate(guid(0xC3)), Err(GptError::NoHeader));
}

/// A primary that parsed cleanly and simply does not contain the target is
/// never compared against the backup — a CRC-verified table is trusted
/// alone, so a `NotFound` is not in the set of errors this falls back on.
#[test]
fn a_valid_primary_that_lacks_the_guid_is_not_retried_against_the_backup() {
    let mut img = disk(mirrored);
    assert_eq!(img.locate(guid(0xEE)), Err(GptError::NotFound { used_entries: 4 }));
}

#[test]
fn a_read_that_does_not_happen_is_an_error() {
    for lba in [0u64, 1, ARRAY_LBA] {
        let mut img = disk(|_| {});
        img.fail_at = Some(lba);
        assert_eq!(img.locate(guid(0xC3)), Err(GptError::ReadFailed(lba)));
    }
}

#[test]
fn block_sizes_outside_the_supported_range() {
    for bad in [0u32, 128, 500, 8192, u32::MAX] {
        let mut img = disk(|_| {});
        img.lba_bytes = bad;
        assert_eq!(img.locate(guid(0xC3)), Err(GptError::UnsupportedLbaSize(bad)));
    }
}

#[test]
fn a_four_kibibyte_block_device_parses() {
    let mut img = disk(|l| {
        (l.lba_bytes, l.lba_count) = (4096, 512);
        (l.primary.first_usable, l.primary.last_usable) = (6, 500);
        l.primary.entries = vec![entry(0, TYPE_OTHER, guid(0x11), 10, 20), entry(1, TYPE_ESP, guid(0x22), 21, 400)];
    });
    let found = img.locate(guid(0x22)).expect("present");
    assert_eq!((found.partition().index(), found.partition().first_lba()), (1, 21));
    assert_eq!(found.used_entries(), 2);
}

#[test]
fn a_device_with_no_room_for_a_table() {
    let mut img = disk(|_| {});
    img.lba_count = 2;
    assert_eq!(img.locate(guid(0xC3)), Err(GptError::DeviceTooSmall(2)));
}

/// Nothing on any path may panic, index out of bounds, or overflow, whatever
/// the disk says. One flipped byte at a time over every byte the parser can
/// reach in the header and the table — 17,920 parses, each of which must
/// simply *return*.
#[test]
fn no_byte_of_the_table_can_panic_the_parser() {
    let mut img = disk(|_| {});
    let reach = (ARRAY_LBA as usize + 32) * LBA as usize;
    let mut located = 0;
    for at in 0..reach {
        for mask in [0x01u8, 0xFF] {
            img.bytes[at] ^= mask;
            if img.locate(guid(0xC3)).is_ok() {
                located += 1;
            }
            img.bytes[at] ^= mask;
        }
    }
    // The sweep has to be able to fail, and a sweep that refused everything
    // would prove nothing about the parser: bytes the table does not read
    // (padding inside entries, the unused tail of the array block) leave a
    // valid table behind, so some of these must still find the partition.
    assert!(located > 0, "every single-byte change broke the table");
    assert!(img.locate(guid(0xC3)).is_ok(), "the sweep did not put the table back");
}

/// The backup GPT's blocks — 2015..=2047 here — are not usable space, and an
/// exact reader concedes none of them for a coarser reader it does not have.
#[test]
fn a_usable_range_reaching_the_backup_gpt_is_refused() {
    let mut img = disk(|l| {
        l.primary.last_usable = DISK_LBAS - 2;
        l.primary.entries[3] = entry(3, TYPE_ESP, guid(0xD4), 300, DISK_LBAS - 2);
        mirrored(l);
    });
    assert_eq!(
        img.locate(guid(0xD4)),
        Err(GptError::UsableRangeCoversBackup {
            last: DISK_LBAS - 2,
            backup_array_lba: DISK_LBAS - 33,
        })
    );

    let mut img = disk(|l| l.primary.last_usable = DISK_LBAS - 34);
    img.locate(guid(0xC3)).expect("the last usable LBA below the mirror was refused");
    let mut img = disk(|l| l.primary.last_usable = DISK_LBAS - 33);
    assert_eq!(
        img.locate(guid(0xC3)),
        Err(GptError::UsableRangeCoversBackup {
            last: DISK_LBAS - 33,
            backup_array_lba: DISK_LBAS - 33,
        })
    );
}

/// The kernel's 4 KiB view floors a 512-byte disk's `lba_count` by up to 7
/// LBAs while an honest table is laid out against the true end — a 2055-LBA
/// disk (2055 % 8 = 7) seen as 2048, its last_usable 2021 at the conceded
/// bound's edge, must parse. The unconceded bound refused every such disk.
#[test]
fn an_honest_table_on_a_floored_device_view_parses() {
    let mut img = disk(|l| {
        l.lba_count = 2055;
        l.primary.last_usable = 2055 - 34;
        mirrored(l);
    });
    (img.lba_count, img.granularity) = (2048, 8);
    let found = img.locate(guid(0xC3)).expect("an honest disk lost /boot");
    assert_eq!(found.partition().index(), 2);
}

/// UEFI gives every entry a `UniquePartitionGUID` that must be unique. Two
/// entries claiming the searched-for GUID must refuse, never resolve
/// first-wins — either one could be the partition the firmware meant.
#[test]
fn two_entries_claiming_the_target_guid_are_refused() {
    let mut img = disk(|l| l.primary.entries[3] = entry(3, TYPE_OTHER, guid(0xC3), 300, 1999));
    assert_eq!(
        img.locate(guid(0xC3)),
        Err(GptError::DuplicateUniqueGuid { first: 2, second: 3 })
    );
    // A duplicate of a GUID nobody asked for does not refuse the answer.
    assert_eq!(img.locate(guid(0xB2)).map(|f| f.partition().index()), Ok(1));
}

/// `entry_count` is the table's own byte: 8 entries make a 2-LBA array, whose
/// first block remains the usable range's exact ceiling.
#[test]
fn a_tiny_entry_array_cannot_buy_the_backup_header() {
    let mut img = disk(|l| {
        (l.primary.entry_count, l.primary.last_usable) = (8, DISK_LBAS - 1);
        l.primary.entries[3] = entry(3, TYPE_ESP, guid(0xD4), 300, DISK_LBAS - 1);
    });
    assert_eq!(
        img.locate(guid(0xD4)),
        Err(GptError::UsableRangeCoversBackup {
            last: DISK_LBAS - 1,
            backup_array_lba: DISK_LBAS - 3,
        })
    );
}

/// The whole table, in entry order: every used entry and none of the unused
/// ones, whatever its type.
#[test]
fn a_list_is_every_used_entry_in_order() {
    let mut img = disk(|_| {});
    let mut out = [None; 8];
    let scan = toyos_gpt::list(&mut img, &mut out).expect("the table parses");
    assert_eq!((scan.matched, scan.used_entries), (4, 4));
    let found: Vec<(u32, Guid)> = out.iter().flatten().flatten().map(|p| (p.index(), p.unique_guid())).collect();
    assert_eq!(
        found,
        vec![(0, guid(0xA1)), (1, guid(0xB2)), (2, guid(0xC3)), (3, guid(0xD4))]
    );
}

/// A scan clears the caller's slice before it fills it: every slot it did not
/// fill is `None`, whatever the caller left there.
#[test]
fn a_list_leaves_no_slot_it_did_not_fill() {
    let mut img = disk(|_| {});
    let bogus = Stated { index: 99, type_guid: TYPE_ESP, unique_guid: guid(0xEE), first: 1, last: 0 };
    let mut out = [Some(Err(bogus)); 8];
    toyos_gpt::list(&mut img, &mut out).expect("the table parses");
    assert_eq!(out.iter().flatten().count(), 4);
}

/// A primary whose array CRC fails is walked before the failure is known, and
/// retried against the backup: nothing the primary's walk put in the slice
/// survives the retry.
#[test]
fn a_backup_retry_leaves_no_slot_of_the_primary() {
    let mut img = disk(mirrored);
    // A fifth used entry, in the primary's array only, so its CRC fails.
    *img.at(ARRAY_LBA, 4 * 128) = 0x01;
    let mut out = [None; 8];
    let scan = toyos_gpt::list(&mut img, &mut out).expect("the backup parses");
    assert_eq!(scan.used_entries, 4);
    assert_eq!(out.iter().flatten().count(), 4);
}
