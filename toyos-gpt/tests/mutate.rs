//! Value-mutated tables with both CRCs resealed: no parse panics, and every answer is true of a copy of the table.

mod table;

use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};

use table::{OnDisk, Image, RawEntry, Rng, Shape};
use toyos_gpt::{GptError, Guid};

/// Iterations of the normal host run; each draws its own seed from its number.
const ITERATIONS: u64 = 5_000;
const SEED: u64 = 0x6770_745F_6D75_7461;

const SHAPE: Shape<'static> = Shape {
    lba_sizes: &[512, 512, 512, 1024, 2048, 4096],
    types: &[Guid::EFI_SYSTEM, Guid::MICROSOFT_BASIC, Guid::TOYOS_DATA, Guid::TOYOS_ROOT],
    entry_sizes: &[128, 128, 128, 128, 256, 512],
    backup: true,
    floored: true,
};

/// A partition as the parser handed it back.
#[derive(Debug)]
struct Seen {
    index: u32,
    type_guid: Guid,
    unique: Guid,
    first: u64,
    last: u64,
    count: u64,
    name: Vec<u16>,
}

/// An entry the parser handed back as no partition.
#[derive(Debug)]
struct Refused {
    index: u32,
    unique: Guid,
    first: u64,
    last: u64,
}

/// What `list` answered: every entry in order, and its two counts.
struct Listed {
    entries: Vec<Result<Seen, Refused>>,
    matched: u32,
    used_entries: u32,
}

/// What `locate` answered for each GUID asked.
type Answers = Vec<(Guid, Result<Seen, GptError>)>;

/// What the parser answered for one image; the one place this file names its API.
fn observe(img: &mut Image, targets: &[Guid]) -> (Result<Listed, GptError>, Answers) {
    let seen = |p: &toyos_gpt::Partition| Seen {
        index: p.index(),
        type_guid: p.type_guid(),
        unique: p.unique_guid(),
        first: p.first_lba(),
        last: p.last_lba(),
        count: p.lba_count().get(),
        name: p.name().to_vec(),
    };
    let mut out = [None; 64];
    let listed = toyos_gpt::list(img, &mut out).map(|scan| Listed {
        entries: out
            .iter()
            .flatten()
            .map(|entry| match entry {
                Ok(p) => Ok(seen(p)),
                Err(u) => Err(Refused { index: u.index, unique: u.unique_guid, first: u.first, last: u.last }),
            })
            .collect(),
        matched: scan.matched,
        used_entries: scan.used_entries,
    });
    let located = targets.iter().map(|&t| (t, toyos_gpt::locate(img, t).map(|l| seen(&l.partition())))).collect();
    (listed, located)
}

fn is(e: &RawEntry, s: &Seen) -> bool {
    let name: Vec<u16> = e.name.iter().copied().take_while(|unit| *unit != 0).collect();
    (e.index, e.type_guid, e.unique, e.first, e.last, &name) == (s.index, s.type_guid, s.unique, s.first, s.last, &s.name)
}

/// A partition's own invariant, against the copy it came from and the device.
fn sound(c: &OnDisk, s: &Seen, lba_count: u64) -> bool {
    c.entry(s.index).is_some_and(|e| is(e, s))
        && c.first_usable <= s.first
        && s.first <= s.last
        && s.last <= c.last_usable
        && s.last < lba_count
        && u128::from(s.count) == u128::from(s.last) - u128::from(s.first) + 1
}

fn overlaps(a: &RawEntry, b: &RawEntry) -> bool {
    a.first <= a.last && b.first <= b.last && a.first <= b.last && b.first <= a.last
}

/// Every property this file states, for one image; `Err` names the one broken.
fn judge(
    copies: &[OnDisk],
    lba_count: u64,
    listed: &Result<Listed, GptError>,
    located: &[(Guid, Result<Seen, GptError>)],
    tally: &mut BTreeMap<String, u64>,
) -> Result<(), String> {
    let mut count = |what: String| *tally.entry(what).or_default() += 1;
    match listed {
        Ok(l) => {
            let faithful = |c: &OnDisk| {
                l.used_entries as usize == c.entries.len()
                    && l.matched as usize == c.entries.len()
                    && l.entries.len() == c.entries.len().min(64)
                    && l.entries.iter().all(|entry| match entry {
                        Ok(s) => sound(c, s, lba_count),
                        Err(r) => c.entry(r.index).is_some_and(|e| {
                            (e.index, e.unique, e.first, e.last) == (r.index, r.unique, r.first, r.last) && !c.places(e)
                        }),
                    })
            };
            if !copies.iter().any(faithful) {
                return Err(format!("list answered what no copy of the table says: {:?}", l.entries));
            }
            for entry in &l.entries {
                count(if entry.is_ok() { "list: partition" } else { "list: no partition" }.into());
            }
        }
        Err(e) => count(format!("list: {}", variant(e))),
    }
    for (target, answer) in located {
        let carrying = |c: &OnDisk| c.entries.iter().filter(|e| e.unique == *target).copied().collect::<Vec<_>>();
        let true_of_a_copy = |c: &OnDisk| match answer {
            Ok(s) => {
                sound(c, s, lba_count)
                    && s.unique == *target
                    && carrying(c).len() == 1
                    && c.entry(s.index).is_some_and(|me| !c.entries.iter().any(|o| o.index != me.index && overlaps(o, me)))
            }
            Err(GptError::PartitionRange { first, last }) => {
                matches!(carrying(c)[..], [e] if (e.first, e.last) == (*first, *last) && !c.places(&e))
            }
            Err(GptError::PartitionOverlap { index }) => match (&carrying(c)[..], c.entry(*index)) {
                ([me], Some(other)) => other.index != me.index && overlaps(other, me) && c.places(me),
                _ => false,
            },
            Err(GptError::DuplicateUniqueGuid { first, second }) => {
                carrying(c).iter().map(|e| e.index).take(2).eq([*first, *second])
            }
            Err(GptError::NotFound { used_entries }) => {
                carrying(c).is_empty() && *used_entries as usize == c.entries.len()
            }
            // Refusals of the table itself, before any entry means anything.
            Err(_) => true,
        };
        let of_the_table = matches!(
            answer,
            Err(GptError::PartitionRange { .. }
                | GptError::PartitionOverlap { .. }
                | GptError::DuplicateUniqueGuid { .. }
                | GptError::NotFound { .. })
                | Ok(_)
        );
        if of_the_table && !copies.iter().any(true_of_a_copy) {
            return Err(format!("locate({target}) answered {answer:?}, which no copy of the table says"));
        }
        count(match answer {
            Ok(_) => "locate: located".into(),
            Err(e) => format!("locate: {}", variant(e)),
        });
    }
    Ok(())
}

fn variant(e: &GptError) -> String {
    let debug = format!("{e:?}");
    debug.split(['(', ' ']).next().unwrap_or_default().to_string()
}

#[test]
fn a_resealed_table_with_any_value_bent_never_panics_and_hands_back_only_partitions() {
    let mut tally = BTreeMap::new();
    for i in 0..ITERATIONS {
        let seed = SEED ^ i.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let mut rng = Rng::new(seed);
        let mut layout = table::valid(&mut rng, &SHAPE);
        // One in ten stays as UEFI laid it out.
        if !rng.one_in(10) {
            table::mutate(&mut rng, &mut layout);
        }
        let mut img = table::image(&layout);
        let mut targets: Vec<Guid> = layout.primary.entries.iter().map(|e| e.unique).collect();
        targets.truncate(3);
        targets.push(rng.guid());

        let run = catch_unwind(AssertUnwindSafe(|| observe(&mut img, &targets)));
        let Ok((listed, located)) = run else {
            panic!("the parser panicked on seed {seed:#x} (iteration {i}): {layout:#?}");
        };
        let copies = OnDisk::both(&img);
        if let Err(why) = judge(&copies, img.lba_count, &listed, &located, &mut tally) {
            panic!("seed {seed:#x} (iteration {i}): {why}\n{layout:#?}");
        }
    }

    for (what, n) in &tally {
        eprintln!("{n:>8}  {what}");
    }
    // Every refusal past both CRCs, and acceptance, has to have happened.
    for reached in [
        "list: partition",
        "list: no partition",
        "locate: located",
        "locate: PartitionRange",
        "locate: PartitionOverlap",
        "locate: DuplicateUniqueGuid",
        "locate: NotFound",
        "locate: UsableRange",
        "locate: UsableRangeCoversBackup",
        "locate: EntryArrayMisplaced",
        "locate: EntryArrayTooBig",
        "locate: EntrySize",
        "locate: HeaderSize",
        "locate: HeaderMisplaced",
        "locate: NoProtectiveMbr",
    ] {
        assert!(tally.get(reached).copied().unwrap_or(0) > 0, "{ITERATIONS} tables never reached {reached:?}");
    }
    let located = |crc: bool| -> u64 {
        tally.iter().filter(|(k, _)| k.starts_with("locate: ") && (!crc || k.ends_with("Crc"))).map(|(_, n)| n).sum()
    };
    let (crc, parses) = (located(true), located(false));
    assert!(crc * 10 < parses, "{crc} of {parses} locates stopped at a checksum: the reseal is not sealing");
}
