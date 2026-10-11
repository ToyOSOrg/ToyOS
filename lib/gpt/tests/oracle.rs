//! toyos-gpt against the `gpt` crate: agreement on UEFI tables, and only named differences on bent ones.

mod table;

use std::collections::BTreeMap;
use std::io::Cursor;

use table::{Image, Layout, Rng, Shape};
use toyos_gpt::{GptError, Guid};

/// Linux filesystem data, a type both readers name.
const LINUX_FS: Guid =
    Guid::from_fields(0x0FC6_3DAF, 0x8483, 0x4772, [0x8E, 0x79, 0x3D, 0x69, 0xD8, 0x47, 0x7D, 0xE4]);

const SHAPE: Shape<'static> = Shape {
    lba_sizes: &[512, 4096],
    types: &[Guid::EFI_SYSTEM, Guid::MICROSOFT_BASIC, LINUX_FS],
    // `gpt` reads 128-byte entries whatever the header says.
    entry_sizes: &[128],
    backup: false,
    floored: false,
};

const VALID: u64 = 2_000;
const BENT: u64 = 4_000;
const SEED: u64 = 0x6770_745F_6F72_636C;

/// One entry: index, type as text, unique GUID, first and last block, name.
type Row = (u32, String, Guid, u64, u64, String);

/// toyos-gpt's reading.
struct Ours {
    disk: Guid,
    placed: BTreeMap<u32, Row>,
    /// Unique GUID, first and last block.
    unplaced: BTreeMap<u32, (Guid, u64, u64)>,
}

fn ours(img: &mut Image) -> Result<Ours, GptError> {
    let mut out = [None; 64];
    let scan = toyos_gpt::list(img, &mut out)?;
    assert!(scan.matched as usize <= out.len(), "a table with more entries than this file's slice");
    let mut read = Ours { disk: scan.disk_guid, placed: BTreeMap::new(), unplaced: BTreeMap::new() };
    for entry in out.iter().flatten() {
        match entry {
            Ok(p) => {
                let row = (p.index(), p.type_guid().to_string(), p.unique_guid(), p.first_lba(), p.last_lba(), String::from_utf16_lossy(p.name()));
                read.placed.insert(p.index(), row);
            }
            Err(u) => {
                read.unplaced.insert(u.index, (u.unique_guid, u.first, u.last));
            }
        }
    }
    Ok(read)
}

/// The `gpt` crate's reading.
enum Theirs {
    Refused(String),
    /// Not asked: it allocates the array its header claims before checking it.
    NotAsked,
    Read { disk: Guid, rows: BTreeMap<u32, Row> },
}

fn theirs(img: &Image) -> Theirs {
    let lb = gpt::disk::LogicalBlockSize::try_from(u64::from(img.lba_bytes)).expect("512 or 4096");
    let mut dev = Cursor::new(img.bytes.as_slice());
    let header = match gpt::header::read_header_from_arbitrary_device(&mut dev, lb) {
        Ok(header) => header,
        Err(e) => return Theirs::Refused(e.to_string()),
    };
    if u64::from(header.num_parts) * u64::from(header.part_size) > 1 << 20 {
        return Theirs::NotAsked;
    }
    match gpt::partition::file_read_partitions(&mut dev, &header, lb) {
        Err(e) => Theirs::Refused(e.to_string()),
        Ok(parts) => Theirs::Read {
            disk: Guid(header.disk_guid.to_bytes_le()),
            // `gpt` keys its entries from 1.
            rows: parts
                .iter()
                .map(|(&key, p)| {
                    let row = (key - 1, p.part_type_guid.guid.to_string(), Guid(p.part_guid.to_bytes_le()), p.first_lba, p.last_lba, p.name.clone());
                    (key - 1, row)
                })
                .collect(),
        },
    }
}

/// The name of the difference between the two readings, or why it has none.
fn differ(layout: &Layout, img: &Image, ours: &Result<Ours, GptError>, theirs: &Theirs) -> Result<&'static str, String> {
    let t = &layout.primary;
    let unnamed = gpt::partition_types::Type::default().guid;
    match (ours, theirs) {
        (Err(_), Theirs::Refused(_)) => Ok("both refuse"),
        (Err(_), Theirs::NotAsked) => Ok("an array over 1 MiB: gpt would allocate it, toyos-gpt refuses"),
        (Ok(_), Theirs::NotAsked) => Err("gpt was not asked, and toyos-gpt read an array over 1 MiB".into()),
        (Err(e), Theirs::Read { .. }) => match e {
            GptError::NoProtectiveMbr => Ok("gpt reads no protective MBR"),
            GptError::UnsupportedRevision(_) | GptError::HeaderReserved(_) | GptError::HeaderMisplaced(_) => {
                Ok("gpt checks no revision, reserved word or header position")
            }
            GptError::HeaderSize(_) | GptError::EntrySize(_) => Ok("gpt checks no header or entry size"),
            GptError::UsableRange { .. }
            | GptError::UsableRangeCoversBackup { .. }
            | GptError::EntryArrayMisplaced { .. }
            | GptError::EntryArrayTooBig { .. } => Ok("gpt checks no layout"),
            GptError::DeviceTooSmall(_) | GptError::ReadFailed(_) if img.lba_count != layout.lba_count => {
                Ok("the device answers a size the image is not")
            }
            other => Err(format!("toyos-gpt refused with {other:?}, and gpt read the table")),
        },
        (Ok(_), Theirs::Refused(why)) if t.header_bytes != 92 => {
            let _ = why;
            Ok("gpt takes the header CRC over 92 bytes whatever header_size says")
        }
        (Ok(_), Theirs::Refused(why)) => Err(format!("toyos-gpt read the table, and gpt refused it: {why}")),
        (Ok(o), Theirs::Read { disk, rows }) => {
            if t.entry_bytes != 128 {
                return Ok("gpt strides 128 bytes whatever the entry size");
            }
            if *disk != o.disk {
                return Err(format!("disk GUID {} against gpt's {disk}", o.disk));
            }
            let mut named = "agree";
            for (index, row) in rows {
                match (o.placed.get(index), o.unplaced.get(index)) {
                    (Some(mine), None) if mine == row => {}
                    // A type only toyos-gpt names: gpt answers the zero GUID.
                    (Some(mine), None) if row.1 == unnamed && (&mine.0, &mine.2, mine.3, mine.4, &mine.5) == (&row.0, &row.2, row.3, row.4, &row.5) => {
                        named = "a type gpt's own table does not name";
                    }
                    (None, Some(stated)) if *stated == (row.2, row.3, row.4) => {
                        named = "an entry toyos-gpt places as no partition: gpt checks no range";
                    }
                    // gpt counts an entry by any non-zero byte, toyos-gpt by its type GUID.
                    (None, None) if row.1 == unnamed => named = "a zero type over non-zero bytes: gpt lists it",
                    (mine, stated) => {
                        return Err(format!("entry {index}: gpt reads {row:?}, toyos-gpt {mine:?} / {stated:?}"));
                    }
                }
            }
            if let Some(index) = o.placed.keys().chain(o.unplaced.keys()).find(|i| !rows.contains_key(i)) {
                return Err(format!("toyos-gpt reads entry {index} and gpt does not"));
            }
            Ok(named)
        }
    }
}

/// The generator's types are ones both readers spell alike.
#[test]
fn both_readers_spell_the_types_alike() {
    for (ours, theirs) in [
        (Guid::EFI_SYSTEM, gpt::partition_types::EFI),
        (Guid::MICROSOFT_BASIC, gpt::partition_types::BASIC),
        (LINUX_FS, gpt::partition_types::LINUX_FS),
    ] {
        assert_eq!(ours.to_string(), theirs.guid);
    }
}

#[test]
fn on_tables_uefi_lays_out_both_readers_agree_entry_for_entry() {
    for i in 0..VALID {
        let seed = SEED ^ i.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let layout = table::valid(&mut Rng::new(seed), &SHAPE);
        let mut img = table::image(&layout);
        let mine = ours(&mut img);
        let named = differ(&layout, &img, &mine, &theirs(&img));
        assert_eq!(named, Ok("agree"), "seed {seed:#x}: {layout:#?}");
        let placed = mine.map(|o| o.placed.len()).unwrap_or_default();
        assert_eq!(placed, layout.primary.entries.len(), "seed {seed:#x}");
    }
}

#[test]
fn on_bent_tables_every_disagreement_is_a_named_difference() {
    let mut tally: BTreeMap<&'static str, u64> = BTreeMap::new();
    for i in 0..BENT {
        let seed = SEED ^ !i.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let mut rng = Rng::new(seed);
        let mut layout = table::valid(&mut rng, &SHAPE);
        table::mutate(&mut rng, &mut layout);
        let mut img = table::image(&layout);
        let mine = ours(&mut img);
        match differ(&layout, &img, &mine, &theirs(&img)) {
            Ok(named) => *tally.entry(named).or_default() += 1,
            Err(why) => panic!("seed {seed:#x}: {why}\n{layout:#?}"),
        }
    }
    for (named, n) in &tally {
        eprintln!("{n:>6}  {named}");
    }
    for reached in ["agree", "both refuse", "an entry toyos-gpt places as no partition: gpt checks no range"] {
        assert!(tally.contains_key(reached), "{BENT} bent tables never reached {reached:?}");
    }
}
