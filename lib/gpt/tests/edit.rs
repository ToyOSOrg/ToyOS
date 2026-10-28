//! The writer: what it writes reads back through both readers, refuses what
//! UEFI 2.10 §5.3 forbids, reports each copy's state, and survives a crash at
//! every point of its write order.

mod table;

use std::collections::BTreeMap;
use std::io::{Cursor, Read, Seek, SeekFrom};

use table::{Image, Rng};
use toyos_gpt::edit::{read_copies, CopyError, Geometry, Invalid, Mbr, Slot, Step, Table, Which};
use toyos_gpt::{GptError, Guid};

const SEED: u64 = 0x6770_745F_6564_6974;

fn image(lba_bytes: u32, lba_count: u64) -> Image {
    Image { lba_bytes, lba_count, bytes: vec![0; (lba_bytes as u64 * lba_count) as usize], fail_at: None }
}

/// One step as a crash model holds it: a run of whole blocks, or a flush.
#[derive(Clone, Debug)]
enum Owned {
    Write { lba: u64, bytes: Vec<u8> },
    Flush,
}

fn owned(table: &Table, geometry: &Geometry, mbr: bool) -> Vec<Owned> {
    let encoded = table.encode(geometry).expect("a valid table");
    encoded
        .steps(mbr)
        .map(|s| match s {
            Step::Write { lba, bytes } => Owned::Write { lba, bytes: bytes.to_vec() },
            Step::Flush => Owned::Flush,
        })
        .collect()
}

fn copy(img: &Image) -> Image {
    Image { lba_bytes: img.lba_bytes, lba_count: img.lba_count, bytes: img.bytes.clone(), fail_at: None }
}

fn apply(img: &mut Image, lba: u64, bytes: &[u8]) {
    let at = (lba * u64::from(img.lba_bytes)) as usize;
    img.bytes[at..at + bytes.len()].copy_from_slice(bytes);
}

fn write(img: &mut Image, steps: &[Owned]) {
    for s in steps {
        if let Owned::Write { lba, bytes } = s {
            apply(img, *lba, bytes);
        }
    }
}

fn slot(rng: &mut Rng, kind: Guid, first: u64, last: u64, name: &str) -> Slot {
    Slot {
        type_guid: kind,
        unique_guid: rng.guid(),
        first,
        last,
        attributes: rng.next() & (1 << 63 | 1),
        name: Slot::named(name).expect("a short name"),
    }
}

/// A table of a few partitions spread over `geometry`, at random indices.
fn some_table(rng: &mut Rng, geometry: &Geometry) -> Table {
    let mut t = Table::empty(geometry, rng.guid());
    let (first, last) = geometry.usable();
    let span = (last - first + 1) / 8;
    let kinds = [Guid::EFI_SYSTEM, Guid::TOYOS_DATA, Guid::TOYOS_ROOT, Guid::MICROSOFT_BASIC];
    for i in 0..rng.range(1, 6) {
        let lo = first + i * span;
        let index = loop {
            let at = rng.below(128) as usize;
            if !t.slots[at].is_used() {
                break at;
            }
        };
        let (kind, len) = (rng.pick(&kinds), rng.below(span));
        t.slots[index] = slot(rng, kind, lo, lo + len, &format!("part {i}"));
    }
    t
}

/// Each partition by index: its type, unique GUID, first and last block.
type Listing = BTreeMap<u32, (Guid, Guid, u64, u64)>;

/// What `toyos_gpt::list`, the reader every disk driver serves from, finds.
fn listed(img: &mut Image) -> Result<Listing, GptError> {
    let mut out = [None; 128];
    toyos_gpt::list(img, &mut out)?;
    Ok(out
        .iter()
        .flatten()
        .map(|e| {
            let p = e.expect("placed");
            (p.index(), (p.type_guid(), p.unique_guid(), p.first_lba(), p.last_lba()))
        })
        .collect())
}

fn listing_of(t: &Table) -> Listing {
    t.used().map(|(i, s)| (i, (s.type_guid, s.unique_guid, s.first, s.last))).collect()
}

/// Every block size the crate parses, on devices of whole 4 KiB blocks and
/// of not whole: both copies read back as the table, and `list` finds it.
#[test]
fn a_written_table_reads_back_from_both_copies() {
    let mut rng = Rng::new(SEED);
    for (lba_bytes, lba_count) in [(512, 262_144), (512, 7_507_812 / 32), (1024, 70_001), (2048, 40_000), (4096, 9_000)] {
        let geometry = Geometry::new(lba_bytes, lba_count).expect("room");
        for _ in 0..20 {
            let t = some_table(&mut rng, &geometry);
            let mut img = image(lba_bytes, lba_count);
            write(&mut img, &owned(&t, &geometry, true));
            let copies = read_copies(&mut img).expect("a geometry");
            assert_eq!(copies.mbr, Mbr::Protective);
            assert_eq!(copies.primary.as_ref(), Ok(&t), "{lba_bytes}x{lba_count}");
            assert!(copies.agree());
            assert_eq!(listed(&mut img), Ok(listing_of(&t)));
            let backup = geometry.backup_header_lba() as usize * lba_bytes as usize;
            assert_eq!(&img.bytes[backup..backup + 8], b"EFI PART", "the backup header is on the last LBA");
        }
    }
}

/// The ROWS a boot image lays, on 1 MiB boundaries, one of them a length the
/// alignment does not round.
const ROWS: [(&str, Guid, u64); 6] = [
    ("ToyOS data", Guid::TOYOS_DATA, 3 << 20),
    ("ESP", Guid::EFI_SYSTEM, 34 << 20),
    ("ToyOS slots", Guid::TOYOS_SLOTS, 1 << 20),
    ("ToyOS log", Guid::MICROSOFT_BASIC, 34 << 20),
    ("ToyOS slot A", Guid::TOYOS_BOOT, (40 << 20) + 4096),
    ("ToyOS root A", Guid::TOYOS_ROOT, 7 << 20),
];

/// **The differential oracle**: the `gpt` crate, a third party's writer
/// sharing no code with this one, lays the same partitions on the same disk
/// under the same GUIDs into the same bytes, on a disk the alignment rounds
/// and on one it does not.
#[test]
fn the_gpt_crate_writes_the_same_table() {
    const LBA: u64 = 512;
    for total in [128u64 << 20, (160 << 20) + 3 * 4096] {
        let mut theirs = vec![0u8; total as usize];
        let mut cursor = Cursor::new(&mut theirs);
        gpt::mbr::ProtectiveMBR::with_lb_size(u32::try_from(total / LBA - 1).unwrap_or(u32::MAX))
            .overwrite_lba0(&mut cursor)
            .expect("the protective MBR");
        let mut disk = gpt::GptConfig::default()
            .initialized(false)
            .writable(true)
            .logical_block_size(gpt::disk::LogicalBlockSize::Lb512)
            .create_from_device(Box::new(cursor), None)
            .expect("a table");
        disk.update_partitions(BTreeMap::new()).expect("an empty table");
        for (name, kind, len) in ROWS {
            let kind = gpt::partition_types::Type { guid: kind.to_string().leak(), os: gpt::partition_types::OperatingSystem::None };
            disk.add_partition(name, len, kind, 0, Some(2048)).expect("a partition");
        }
        let mut device = disk.write().expect("the table");
        let mut theirs = vec![0u8; total as usize];
        device.seek(SeekFrom::Start(0)).and_then(|_| device.read_exact(&mut theirs)).expect("read back");
        let guid = |at: usize| Guid(theirs[at..at + 16].try_into().unwrap());

        let geometry = Geometry::new(LBA as u32, total / LBA).expect("room");
        let mut t = Table::empty(&geometry, guid(LBA as usize + 56));
        let mut at = 2048;
        for (i, (name, kind, len)) in ROWS.iter().enumerate() {
            let first = at;
            at = (first + len / LBA).next_multiple_of(2048);
            t.slots[i] = Slot {
                type_guid: *kind,
                unique_guid: guid(2 * LBA as usize + i * 128 + 16),
                first,
                last: first + len / LBA - 1,
                attributes: 0,
                name: Slot::named(name).unwrap(),
            };
        }
        let mut ours = image(LBA as u32, total / LBA);
        write(&mut ours, &owned(&t, &geometry, true));
        let differing = (0..total as usize).filter(|&i| ours.bytes[i] != theirs[i]).count();
        assert_eq!(differing, 0, "{total}: {differing} bytes differ from the gpt crate's");
    }
}

/// Each thing §5.3 forbids is refused by name, before a byte is encoded.
#[test]
fn a_table_uefi_forbids_is_refused() {
    let mut rng = Rng::new(SEED ^ 1);
    let g = Geometry::new(512, 100_000).unwrap();
    let (first, last) = g.usable();
    assert_eq!((first, last), (34, 100_000 - 34), "§5.3.1's minimum at 512-byte blocks");
    assert_eq!(Geometry::new(4096, 100).unwrap().usable(), (6, 100 - 6), "and at 4096");
    let base = |rng: &mut Rng| {
        let mut t = Table::empty(&g, rng.guid());
        t.slots[0] = slot(rng, Guid::TOYOS_DATA, 2048, 4095, "a");
        t.slots[5] = slot(rng, Guid::TOYOS_ROOT, 4096, 8191, "b");
        t
    };
    assert_eq!(base(&mut rng).check(&g), Ok(()));
    type Case = (&'static str, fn(&mut Table), Invalid);
    let cases: [Case; 8] = [
        ("zero disk", |t| t.disk_guid = Guid::ZERO, Invalid::DiskGuid),
        ("usable over the primary", |t| t.first_usable = 33, Invalid::UsableRange { first: 33, last: 100_000 - 34 }),
        ("usable over the backup", |t| t.last_usable += 1, Invalid::UsableRange { first: 34, last: 100_000 - 33 }),
        ("reversed", |t| t.slots[5].last = 4000, Invalid::OutsideUsable { index: 5 }),
        ("past usable", |t| t.slots[5].last = 100_000 - 33, Invalid::OutsideUsable { index: 5 }),
        ("overlap", |t| t.slots[5].first = 4095, Invalid::Overlap { first: 0, second: 5 }),
        ("zero unique", |t| t.slots[0].unique_guid = Guid::ZERO, Invalid::ZeroUnique { index: 0 }),
        ("unused not zero", |t| t.slots[9].first = 1, Invalid::UnusedNotZero { index: 9 }),
    ];
    for (what, bend, want) in cases {
        let mut t = base(&mut rng);
        bend(&mut t);
        assert_eq!(t.check(&g), Err(want), "{what}");
        assert!(t.encode(&g).is_err(), "{what} encoded");
    }
    let mut t = base(&mut rng);
    t.slots[5].unique_guid = t.slots[0].unique_guid;
    assert_eq!(t.check(&g), Err(Invalid::DuplicateUnique { first: 0, second: 5 }));
    assert_eq!(Slot::named(&"x".repeat(36)).map(|n| n[35]), Some(u16::from(b'x')));
    assert_eq!(Slot::named(&"x".repeat(37)), None, "37 units");
}

/// What each copy holds is reported alone: a torn primary leaves the backup
/// in force, a torn backup leaves the primary in force and the two not in
/// agreement, two valid copies that differ do not agree, an MBR disk and a
/// blank one are named, and an array of another shape is not read as one.
#[test]
fn each_copy_is_read_and_reported_alone() {
    let mut rng = Rng::new(SEED ^ 2);
    let g = Geometry::new(512, 50_000).unwrap();
    let t = some_table(&mut rng, &g);
    let mut whole = image(512, 50_000);
    write(&mut whole, &owned(&t, &g, true));
    let header = |which: Which| match which {
        Which::Primary => 512,
        Which::Backup => g.backup_header_lba() as usize * 512,
    };

    for which in [Which::Primary, Which::Backup] {
        let mut torn = copy(&whole);
        torn.bytes[header(which) + 40] ^= 1;
        let copies = read_copies(&mut torn).unwrap();
        let other = if which == Which::Primary { Which::Backup } else { Which::Primary };
        assert_eq!(copies.in_force().map(|(w, t)| (w, t.clone())), Some((other, t.clone())), "{which:?} torn");
        assert!(!copies.agree());
        let bad = if which == Which::Primary { &copies.primary } else { &copies.backup };
        assert!(matches!(bad, Err(CopyError::Gpt(GptError::HeaderCrc { .. }))), "{bad:?}");
    }

    // A second valid table in the backup's place alone.
    let mut changed = t;
    changed.slots.iter_mut().find(|s| s.is_used()).unwrap().name = Slot::named("renamed").unwrap();
    let mut differing = copy(&whole);
    let steps = owned(&changed, &g, false);
    write(&mut differing, &steps[..2]);
    let copies = read_copies(&mut differing).unwrap();
    assert_eq!(copies.in_force().map(|(w, _)| w), Some(Which::Primary));
    assert!(copies.primary.is_ok() && copies.backup.is_ok() && !copies.agree());

    let mut mbr = image(512, 50_000);
    mbr.bytes[446 + 4] = 0x83;
    mbr.bytes[510..512].copy_from_slice(&[0x55, 0xAA]);
    let copies = read_copies(&mut mbr).unwrap();
    assert_eq!((copies.mbr, copies.in_force()), (Mbr::Other, None));
    assert_eq!(read_copies(&mut image(512, 50_000)).unwrap().mbr, Mbr::Blank);
    assert!(read_copies(&mut image(512, 66)).is_none(), "no room for both copies and a block");

    // The same table with 256-byte entries, resealed: valid, and not this shape.
    let mut wide = copy(&whole);
    let h = 512;
    wide.bytes[h + 80..h + 84].copy_from_slice(&64u32.to_le_bytes());
    wide.bytes[h + 84..h + 88].copy_from_slice(&256u32.to_le_bytes());
    wide.bytes[h + 16..h + 20].fill(0);
    let crc = toyos_gpt::crc32(&wide.bytes[h..h + 92]);
    wide.bytes[h + 16..h + 20].copy_from_slice(&crc.to_le_bytes());
    let copies = read_copies(&mut wide).unwrap();
    assert_eq!(copies.primary, Err(CopyError::Shape { entries: 64, entry_bytes: 256 }));
}

/// What a crash may leave of one write: nothing, all of it, or any subset of
/// its blocks.
#[derive(Clone, Copy, Debug)]
enum Landed {
    None,
    Whole,
    Torn(u64),
}

/// Show `each` every image a crash during `steps` over `old` may leave, and
/// answer how many. A flush is a barrier: everything before it is on the
/// medium. Between two flushes each write landed whole, not at all, or torn
/// into an arbitrary subset of its blocks; `torn` draws that many subsets
/// per write.
fn crashes(old: &Image, steps: &[Owned], rng: &mut Rng, torn: usize, each: &mut dyn FnMut(&str, &mut Image)) -> usize {
    let epochs: Vec<&[Owned]> = steps.split(|s| matches!(s, Owned::Flush)).collect();
    let mut seen = 0;
    let mut base = copy(old);
    for (e, epoch) in epochs.iter().enumerate() {
        let writes: Vec<(u64, &Vec<u8>)> =
            epoch.iter().filter_map(|s| if let Owned::Write { lba, bytes } = s { Some((*lba, bytes)) } else { None }).collect();
        let choices: Vec<Vec<Landed>> = writes
            .iter()
            .map(|_| {
                let mut c = vec![Landed::None, Landed::Whole];
                c.extend((0..torn).map(|_| Landed::Torn(rng.next())));
                c
            })
            .collect();
        let mut pick = vec![0usize; writes.len()];
        loop {
            let mut img = copy(&base);
            for ((lba, bytes), (c, k)) in writes.iter().zip(choices.iter().zip(&pick)) {
                let blocks = bytes.chunks(old.lba_bytes as usize).enumerate();
                for (i, b) in blocks {
                    let lands = match c[*k] {
                        Landed::None => false,
                        Landed::Whole => true,
                        Landed::Torn(mask) => mask.rotate_left(i as u32) & 1 == 1,
                    };
                    if lands {
                        apply(&mut img, lba + i as u64, b);
                    }
                }
            }
            let at = format!("epoch {e} {:?}", pick.iter().zip(&choices).map(|(k, c)| c[*k]).collect::<Vec<_>>());
            each(&at, &mut img);
            seen += 1;
            let Some(next) = (0..pick.len()).find(|&i| pick[i] + 1 < choices[i].len()) else { break };
            pick[next] += 1;
            pick[..next].fill(0);
        }
        for (lba, bytes) in writes {
            apply(&mut base, lba, bytes);
        }
    }
    seen
}

/// Whether a crash left an image read as wholly the old table or wholly the
/// new one by the reader drivers serve from, and §5.3.2's order held: the
/// primary is never newer than the backup.
fn judge(img: &mut Image, old: &Table, new: &Table) -> Result<(), String> {
    let seen = listed(img).map_err(|e| format!("no table: {e:?}"))?;
    if seen != listing_of(old) && seen != listing_of(new) {
        return Err(format!("neither table: {seen:?}"));
    }
    let copies = read_copies(img).ok_or("no geometry")?;
    if copies.primary.as_ref() == Ok(new) && copies.backup.as_ref() != Ok(new) {
        return Err("the primary is new and the backup is not".into());
    }
    Ok(())
}

/// The write order's whole claim: a crash anywhere — between any two writes,
/// or inside one, any subset of its blocks landed — leaves the disk reading
/// as the old table or the new one, never neither and never a mixture, and
/// the primary never newer than the backup.
#[test]
fn a_crash_anywhere_in_the_write_reads_as_the_old_table_or_the_new() {
    let mut rng = Rng::new(SEED ^ 3);
    for (lba_bytes, lba_count) in [(512u32, 40_000u64), (4096, 5_000)] {
        let g = Geometry::new(lba_bytes, lba_count).unwrap();
        for _ in 0..4 {
            let old = some_table(&mut rng, &g);
            let mut new = some_table(&mut rng, &g);
            new.disk_guid = old.disk_guid;
            let mut disk = image(lba_bytes, lba_count);
            write(&mut disk, &owned(&old, &g, true));
            let steps = owned(&new, &g, false);
            let states = crashes(&disk, &steps, &mut rng, 6, &mut |at, img| {
                if let Err(why) = judge(img, &old, &new) {
                    panic!("{lba_bytes}x{lba_count}, a crash at {at}: {why}");
                }
            });
            assert!(states > 100, "{states} crash states");
        }
    }
}
