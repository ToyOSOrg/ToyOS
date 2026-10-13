use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use super::container::{patch_for, Refused};
use super::ladder::{self, Bench, Bytes, Cpu, Lost, Outcome, Placement, Reading, Staged, Step, Why, Write, Wrote};
use super::smbios;

/// AMD's Family 17h container, pinned by its digest in `NOTICE`.
const FAM17H: &[u8] = include_bytes!("../../microcode/amd-ucode/microcode_amd_fam17h.bin");
const BENCH: u32 = 0x0086_0f01;
const BENCH_LEVEL: u32 = 0x0860_010d;

// --- The container -----------------------------------------------------------

#[test]
fn the_bench_patch_is_the_block_at_0x262c() {
    let patch = patch_for(FAM17H, BENCH).unwrap().unwrap();
    assert_eq!((patch.offset, patch.block.len()), (0x262c, 3200));
    assert_eq!((patch.level, patch.equivalence), (BENCH_LEVEL, 0x8601));
    // The header's date, then its level, as the file holds them.
    assert_eq!(patch.block[..8], [0x24, 0x20, 0x23, 0x02, 0x0d, 0x01, 0x60, 0x08]);
}

#[test]
fn every_signature_the_table_names_has_its_one_patch() {
    let table = [
        (0x0087_0f10, 0x0870_1034, 0x8710),
        (0x0080_0f12, 0x0800_126f, 0x8012),
        (0x0083_0f10, 0x0830_107c, 0x8310),
        (0x0086_0f01, 0x0860_010d, 0x8601),
        (0x0080_0f82, 0x0800_820d, 0x8082),
        (0x008a_0f00, 0x08a0_000a, 0x8a00),
        (0x0086_0f81, 0x0860_8108, 0x8681),
    ];
    for (signature, level, id) in table {
        let patch = patch_for(FAM17H, signature).unwrap().unwrap();
        assert_eq!((patch.level, patch.equivalence, patch.block.len()), (level, id, 3200), "{signature:#x}");
    }
    // The T14's Intel signature, and a 17h one AMD's file does not name.
    assert_eq!(patch_for(FAM17H, 0x0008_06c1), Ok(None));
    assert_eq!(patch_for(FAM17H, 0x0086_0f00), Ok(None));
}

/// A container as AMD's own writer emits one: the entries, an all-zero
/// entry, then each patch's section.
fn container(entries: &[(u32, u16)], patches: &[(u16, u32, usize)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"DMA\0");
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(16 * (entries.len() as u32 + 1)).to_le_bytes());
    for &(signature, id) in entries {
        out.extend_from_slice(&signature.to_le_bytes());
        out.extend_from_slice(&[0; 8]);
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&[0; 2]);
    }
    out.extend_from_slice(&[0; 16]);
    for &(id, level, len) in patches {
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&(len as u32).to_le_bytes());
        let mut block = vec![0xa5; len];
        block[4..8].copy_from_slice(&level.to_le_bytes());
        block[0x18..0x1c].copy_from_slice(&u32::from(id).to_le_bytes());
        out.extend_from_slice(&block);
    }
    out
}

fn malformed(file: &[u8]) -> &'static str {
    match patch_for(file, BENCH) {
        Err(Refused::Malformed { why, .. }) => why,
        other => panic!("not refused as malformed: {other:?}"),
    }
}

#[test]
fn a_file_is_refused_whole_on_any_bound_it_breaks() {
    let good = container(&[(BENCH, 0x8601)], &[(0x8601, 7, 64)]);
    assert_eq!(patch_for(&good, BENCH).unwrap().unwrap().level, 7);

    let mut bad = good.clone();
    bad[3] = b'!';
    assert_eq!(malformed(&bad), "no container magic");
    let mut bad = good.clone();
    bad[4] = 1;
    assert_eq!(malformed(&bad), "the first section is not the equivalence table");
    let mut bad = good.clone();
    bad[8] = 24;
    assert_eq!(malformed(&bad), "the table is not whole entries");
    let mut bad = good.clone();
    bad[8] = 0;
    assert_eq!(malformed(&bad), "the table is not whole entries");
    let mut bad = good.clone();
    bad[8..12].copy_from_slice(&0x1000u32.to_le_bytes());
    assert_eq!(malformed(&bad), "the table runs past the end of the file");
    let mut bad = good.clone();
    bad[12 + 16] = 1;
    assert_eq!(malformed(&bad), "the table does not end in an all-zero entry");
    let bad = container(&[(0, 0x8000), (BENCH, 0x8601)], &[]);
    assert_eq!(malformed(&bad), "an all-zero signature before the table's end");

    let section = 12 + 32;
    let mut bad = good.clone();
    bad[section] = 2;
    assert_eq!(malformed(&bad), "a section that is not a patch");
    let mut bad = good.clone();
    bad[section + 4] = 65;
    assert_eq!(malformed(&bad), "a patch runs past the end of the file");
    let short = container(&[(BENCH, 0x8601)], &[(0x8601, 7, 31)]);
    assert_eq!(malformed(&short), "a patch shorter than its header");
    let mut trailing = good;
    trailing.extend_from_slice(&[1, 0, 0]);
    assert_eq!(malformed(&trailing), "bytes after the last section");
}

#[test]
fn the_cpu_takes_one_patch_from_any_container_of_a_file() {
    let mut file = container(&[(0x0080_0f12, 0x8012)], &[(0x8012, 1, 64)]);
    file.extend(container(&[(BENCH, 0x8601)], &[(0x8012, 2, 64), (0x8601, 3, 80)]));
    let patch = patch_for(&file, BENCH).unwrap().unwrap();
    assert_eq!((patch.level, patch.block.len()), (3, 80));
    // The first container's table does not name the bench, so its patch for
    // 0x8012 is no patch for it; nor is the second's for 0x8012.
    assert_eq!(patch_for(&file, 0x0080_0f12).unwrap().unwrap().level, 1);

    let two = container(&[(BENCH, 0x8601)], &[(0x8601, 3, 64), (0x8601, 4, 64)]);
    assert!(matches!(patch_for(&two, BENCH), Err(Refused::TwoPatches { .. })));
}

#[test]
fn equivalence_id_zero_names_nothing() {
    let file = container(&[(BENCH, 0)], &[(0, 3, 64)]);
    assert_eq!(patch_for(&file, BENCH), Ok(None));
}

#[test]
fn no_prefix_or_corruption_of_the_real_file_panics() {
    for len in 0..FAM17H.len() {
        let _ = patch_for(&FAM17H[..len], BENCH);
    }
    let mut file = FAM17H.to_vec();
    for at in (0..0x100).chain(0x2600..0x2680) {
        for bit in [0x01, 0x80] {
            file[at] ^= bit;
            let _ = patch_for(&file, BENCH);
            file[at] ^= bit;
        }
    }
}

// --- SMBIOS ------------------------------------------------------------------

fn sealed(mut bytes: Vec<u8>, sum_at: usize, over: core::ops::Range<usize>) -> Vec<u8> {
    let sum = bytes[over].iter().fold(0u8, |s, &b| s.wrapping_add(b));
    bytes[sum_at] = bytes[sum_at].wrapping_sub(sum);
    bytes
}

fn entry_3(table: u64, max: u32) -> Vec<u8> {
    let mut e = vec![0u8; 0x18];
    e[..5].copy_from_slice(b"_SM3_");
    e[6] = 0x18;
    e[7] = 3;
    e[8] = 2;
    e[0x0c..0x10].copy_from_slice(&max.to_le_bytes());
    e[0x10..0x18].copy_from_slice(&table.to_le_bytes());
    sealed(e, 5, 0..0x18)
}

fn entry_2(table: u32, len: u16) -> Vec<u8> {
    let mut e = vec![0u8; 0x1f];
    e[..4].copy_from_slice(b"_SM_");
    e[5] = 0x1f;
    e[6] = 2;
    e[7] = 8;
    e[0x10..0x15].copy_from_slice(b"_DMI_");
    e[0x16..0x18].copy_from_slice(&len.to_le_bytes());
    e[0x18..0x1c].copy_from_slice(&table.to_le_bytes());
    let e = sealed(e, 0x15, 0x10..0x1f);
    sealed(e, 4, 0..0x1f)
}

#[test]
fn either_entry_point_names_its_table() {
    let three = smbios::entry_point(&entry_3(0x7a3e_9000, 0x1234)).unwrap();
    assert_eq!(three, smbios::EntryPoint { major: 3, minor: 2, table: 0x7a3e_9000, len: 0x1234 });
    let huge = smbios::entry_point(&entry_3(0x1000, u32::MAX)).unwrap();
    assert_eq!(huge.len, smbios::MAX_TABLE_LEN);
    let two = smbios::entry_point(&entry_2(0x000f_0000, 0x0800)).unwrap();
    assert_eq!(two, smbios::EntryPoint { major: 2, minor: 8, table: 0xf_0000, len: 0x800 });
}

#[test]
fn an_entry_point_that_does_not_hold_is_refused() {
    let mut bad = entry_3(0x1000, 0x100);
    bad[0x10] ^= 1;
    assert_eq!(smbios::entry_point(&bad), Err("the entry point's checksum does not hold"));
    assert_eq!(smbios::entry_point(&entry_3(0x1000, 0x100)[..0x17]), Err("the entry point is cut short"));
    let mut short = entry_3(0x1000, 0x100);
    short[6] = 0x10;
    assert_eq!(smbios::entry_point(&short), Err("a 3.0 entry point shorter than 0x18 bytes"));

    let mut bad = entry_2(0x1000, 0x100);
    bad[0x11] = b'X';
    bad[4] = bad[4].wrapping_sub(b'X'.wrapping_sub(b'D'));
    assert_eq!(smbios::entry_point(&bad), Err("a 2.1 entry point without its intermediate anchor"));
    let mut bad = entry_2(0x1000, 0x100);
    bad[0x18] ^= 1;
    assert_eq!(smbios::entry_point(&bad), Err("the entry point's checksum does not hold"));
    assert_eq!(smbios::entry_point(b"_SN3_ and the rest of it......"), Err("no SMBIOS anchor"));
}

/// A structure of `kind` with `formatted` after its four header bytes, then
/// `strings`.
fn structure(kind: u8, formatted: &[u8], strings: &[&str]) -> Vec<u8> {
    let mut out = vec![kind, 4 + formatted.len() as u8, 0x34, 0x12];
    out.extend_from_slice(formatted);
    for s in strings {
        out.extend_from_slice(s.as_bytes());
        out.push(0);
    }
    if strings.is_empty() {
        out.push(0);
    }
    out.push(0);
    out
}

/// A type 0 of SMBIOS 2.4's length: vendor 1, version 2, date 3, at their
/// offsets less the four header bytes.
fn bios_info() -> Vec<u8> {
    let mut formatted = vec![0u8; 0x14];
    formatted[0x00] = 1;
    formatted[0x01] = 2;
    formatted[0x04] = 3;
    formatted[0x10..0x14].copy_from_slice(&[1, 56, 1, 38]);
    structure(0, &formatted, &["LENOVO", "DMCN38WW", "06/02/2022"])
}

#[test]
fn the_bios_information_says_who_made_the_firmware_and_which_it_is() {
    let mut table = structure(1, &[0; 4], &[]);
    table.extend(bios_info());
    table.extend(structure(11, &[1], &["AGESA!V9 RenoirPI-FP6 1.0.0.7", "SPI flash 16MB"]));
    table.extend(structure(127, &[], &[]));
    table.extend(structure(0, &[9; 20], &["after the end"]));

    let bios = smbios::bios(&table).unwrap();
    assert_eq!(bios.vendor, Some(&b"LENOVO"[..]));
    assert_eq!(bios.version, Some(&b"DMCN38WW"[..]));
    assert_eq!(bios.date, Some(&b"06/02/2022"[..]));
    assert_eq!((bios.release, bios.ec), (Some((1, 56)), Some((1, 38))));
    assert_eq!(smbios::structures(&table).count(), 4);

    let named: Vec<&[u8]> = smbios::platform_versions(&table).collect();
    assert_eq!(named, [&b"AGESA!V9 RenoirPI-FP6 1.0.0.7"[..]]);
}

#[test]
fn a_platform_version_is_named_by_agesa_or_a_pi_word() {
    let table = structure(11, &[2], &["ComboAM4v2PI 1.2.0.E", "RenoirPI-FP6 1.0.0.Eb", "SPI flash", "API-2", "PIPI", "AGESA"]);
    let named: Vec<&[u8]> = smbios::platform_versions(&table).collect();
    assert_eq!(named, [&b"ComboAM4v2PI 1.2.0.E"[..], b"RenoirPI-FP6 1.0.0.Eb", b"AGESA"]);
}

#[test]
fn a_string_number_names_a_string_or_nothing() {
    let mut formatted = vec![0u8; 0x0e];
    formatted[0] = 0; // vendor: no string
    formatted[1] = 7; // version: past the set
    formatted[4] = 1; // date
    let table = structure(0, &formatted, &["only"]);
    let bios = smbios::bios(&table).unwrap();
    assert_eq!((bios.vendor, bios.version, bios.date), (None, None, Some(&b"only"[..])));
    // A 2.0 structure has no release fields.
    assert_eq!((bios.release, bios.ec), (None, None));
}

#[test]
fn a_malformed_table_is_read_only_as_far_as_it_holds() {
    let mut table = bios_info();
    table.extend([0, 2, 0, 0, 0, 0]);
    assert_eq!(smbios::structures(&table).count(), 1);
    let unterminated = &bios_info()[..bios_info().len() - 1];
    assert_eq!(smbios::structures(unterminated).count(), 0);
    let overlong = [0u8, 200, 0, 0, 0, 0];
    assert_eq!(smbios::structures(&overlong).count(), 0);
    for len in 0..bios_info().len() {
        let _ = smbios::bios(&bios_info()[..len]);
    }
}

#[test]
fn firmware_bytes_are_quoted_with_everything_unprintable_escaped() {
    let line = format!("{}", smbios::Quoted(Some(b"A \"b\"\\\x07\n\xff")));
    assert_eq!(line, r#""A \"b\"\\\x07\x0a\xff""#);
    assert_eq!(format!("{}", smbios::Quoted(None)), "none");
}

// --- The ladder --------------------------------------------------------------

/// A machine of `cores` cores of `threads` threads, CPUs numbered thread by
/// thread within a core, and one level register per core.
struct Fake {
    cpus: Vec<Cpu>,
    levels: Vec<u32>,
    /// What a load does to `before`: the fault and the level after.
    loads: fn(Staged, u32) -> (Option<u32>, u32),
    said: Vec<Reading>,
    writes: usize,
    lost: Option<u32>,
    /// What an idle period leaves each core at.
    idle: fn(u32) -> u32,
}

impl Fake {
    fn new(cores: u32, threads: u32, level: u32) -> Self {
        let cpus = (0..cores * threads).map(|id| Cpu { id, apic: id, core: id / threads }).collect();
        Self { cpus, levels: vec![level; cores as usize], loads: documented, said: Vec::new(), writes: 0, lost: None, idle: |l| l }
    }

    fn core(&self, cpu: u32) -> usize {
        self.cpus[cpu as usize].core as usize
    }

    fn answer(&self, cpu: u32) -> Result<(), Lost> {
        if self.lost == Some(cpu) { Err(Lost(cpu)) } else { Ok(()) }
    }

    fn lines(&self) -> Vec<String> {
        self.said.iter().map(|r| format!("{r}")).collect()
    }

    fn steps(&self) -> Vec<(Step, u32)> {
        self.said
            .iter()
            .filter_map(|r| match r {
                Reading::Wrote { step, cpu, .. } => Some((*step, cpu.id)),
                _ => None,
            })
            .collect()
    }
}

/// The CPU as the documents read: an intact patch raises a lower level to
/// its own and leaves an equal one, anything else is a #GP.
fn documented(staged: Staged, before: u32) -> (Option<u32>, u32) {
    match staged.bytes {
        Bytes::Intact if before < BENCH_LEVEL => (None, BENCH_LEVEL),
        Bytes::Intact => (None, before),
        Bytes::BodyFlipped | Bytes::LevelFlipped => (Some(0), before),
    }
}

impl Bench for Fake {
    fn cpus(&self) -> &[Cpu] {
        &self.cpus
    }

    fn level(&mut self, cpu: u32) -> Result<u32, Lost> {
        self.answer(cpu)?;
        Ok(self.levels[self.core(cpu)])
    }

    fn load(&mut self, cpu: u32, staged: Staged) -> Result<Write, Lost> {
        self.answer(cpu)?;
        self.writes += 1;
        let core = self.core(cpu);
        let before = self.levels[core];
        let (fault, after) = (self.loads)(staged, before);
        self.levels[core] = after;
        Ok(Write { at: 0x1000, before, fault, after, ticks: 9, nanos: 3 })
    }

    fn write_level(&mut self, cpu: u32) -> Result<Write, Lost> {
        self.answer(cpu)?;
        self.writes += 1;
        let before = self.levels[self.core(cpu)];
        Ok(Write { at: 0, before, fault: Some(0), after: before, ticks: 1, nanos: 1 })
    }

    fn idle(&mut self) -> Result<u64, Lost> {
        for level in &mut self.levels {
            *level = (self.idle)(*level);
        }
        Ok(2000)
    }

    fn say(&mut self, reading: Reading) {
        self.said.push(reading);
    }
}

const FIRMWARE: u32 = 0x0860_0106;

#[test]
fn a_cpu_at_or_above_the_patch_stops_the_ladder_before_any_write() {
    let mut fake = Fake::new(4, 2, FIRMWARE);
    fake.levels[2] = BENCH_LEVEL;
    ladder::run(&mut fake, BENCH_LEVEL);
    assert_eq!(fake.writes, 0);
    let m0 = fake.said.iter().filter(|r| matches!(r, Reading::Level { step: Step::M0, .. })).count();
    assert_eq!(m0, 8, "every CPU's level is read before the stop");
    assert_eq!(
        fake.said.last(),
        Some(&Reading::Stopped { step: Step::M0, why: Why::AtOrAbove { cpu: 4, level: BENCH_LEVEL, patch: BENCH_LEVEL } })
    );
}

#[test]
fn a_first_load_that_does_not_take_stops_the_ladder() {
    for loads in [
        (|_, before| (Some(0), before)) as fn(Staged, u32) -> (Option<u32>, u32),
        |_, before| (None, before),
        |_, _| (None, 0x0860_0107),
    ] {
        let mut fake = Fake::new(4, 2, FIRMWARE);
        fake.loads = loads;
        ladder::run(&mut fake, BENCH_LEVEL);
        assert_eq!(fake.steps(), [(Step::Guard, 0), (Step::M1, 0)]);
        assert_eq!(fake.said.last(), Some(&Reading::Stopped { step: Step::M1, why: Why::NotLoaded }));
    }
}

#[test]
fn the_whole_ladder_runs_in_order_on_the_cores_it_needs() {
    let mut fake = Fake::new(4, 2, FIRMWARE);
    ladder::run(&mut fake, BENCH_LEVEL);
    // M0 for all eight before the first write.
    let first_write = fake.said.iter().position(|r| matches!(r, Reading::Wrote { .. })).unwrap();
    assert_eq!(first_write, 8);
    assert_eq!(
        fake.steps(),
        [
            (Step::Guard, 0),
            (Step::M1, 0),
            // A core nothing has written, each.
            (Step::M2, 2),
            (Step::M2, 4),
            (Step::M3, 0),
            (Step::M4, 1),
            // The last unwritten core, then the first still below the patch.
            (Step::M5, 6),
            (Step::M5, 2),
            // Every CPU not yet handed the intact patch.
            (Step::Load, 3),
            (Step::Load, 4),
            (Step::Load, 5),
            (Step::Load, 7),
        ]
    );
    // M4 reads the sibling before it writes, and it reads the core's level.
    assert!(fake.said.contains(&Reading::Level {
        step: Step::M4,
        cpu: Cpu { id: 1, apic: 1, core: 0 },
        level: BENCH_LEVEL
    }));
    let outcome = |step, cpu| {
        fake.said.iter().find_map(|r| match r {
            Reading::Wrote { step: s, cpu: c, outcome, .. } if *s == step && c.id == cpu => Some(*outcome),
            _ => None,
        })
    };
    assert_eq!(outcome(Step::M1, 0), Some(Outcome::Named));
    assert_eq!(outcome(Step::M2, 2), Some(Outcome::Gp));
    assert_eq!(outcome(Step::M3, 0), Some(Outcome::Unchanged));
    assert_eq!(outcome(Step::M5, 6), Some(Outcome::Named));
    let idled = fake.said.iter().filter(|r| matches!(r, Reading::Idled { level: BENCH_LEVEL, .. })).count();
    assert_eq!(idled, 8);
    assert_eq!(fake.said.last(), Some(&Reading::Done));
}

#[test]
fn a_one_core_machine_skips_what_needs_another_core() {
    let mut fake = Fake::new(1, 1, FIRMWARE);
    ladder::run(&mut fake, BENCH_LEVEL);
    assert_eq!(fake.steps(), [(Step::Guard, 0), (Step::M1, 0), (Step::M3, 0)]);
    let skipped: Vec<(Step, Why)> = fake
        .said
        .iter()
        .filter_map(|r| match r {
            Reading::Skipped { step, why } => Some((*step, *why)),
            _ => None,
        })
        .collect();
    assert_eq!(
        skipped,
        [
            (Step::M2, Why::NoUnwrittenCore),
            (Step::M2, Why::NoUnwrittenCore),
            (Step::M4, Why::NoSibling),
            (Step::M5, Why::NoCoreBelow),
            (Step::M5, Why::NoCoreBelow),
        ]
    );
    assert_eq!(fake.said.last(), Some(&Reading::Done));
}

#[test]
fn a_cpu_that_does_not_answer_ends_the_reading_naming_it() {
    let mut fake = Fake::new(2, 2, FIRMWARE);
    fake.lost = Some(3);
    ladder::run(&mut fake, BENCH_LEVEL);
    assert_eq!(fake.writes, 0);
    assert_eq!(fake.said.last(), Some(&Reading::Stopped { step: Step::M0, why: Why::Lost { cpu: 3 } }));
}

#[test]
fn a_level_lost_while_halted_is_read_as_such() {
    let mut fake = Fake::new(2, 2, FIRMWARE);
    fake.idle = |_| 0;
    ladder::run(&mut fake, BENCH_LEVEL);
    let lines = fake.lines();
    assert!(lines.contains(&String::from("amducode: M6 cpu3 apic 3 core 1 after 2000 ms halted level 0x0")), "{lines:#?}");
}

#[test]
fn each_reading_is_one_line_a_metal_row_reads() {
    let cpu = Cpu { id: 2, apic: 4, core: 1 };
    let write = Write { at: 0xffff_8000_0012_300c, before: FIRMWARE, fault: None, after: BENCH_LEVEL, ticks: 51234, nanos: 17078 };
    let staged = Staged { placement: Placement::InPlace, bytes: Bytes::Intact };
    let lines = [
        Reading::Level { step: Step::M0, cpu, level: FIRMWARE },
        Reading::Wrote { step: Step::M5, cpu, what: Wrote::Loader(staged), write, outcome: Outcome::Named },
        Reading::Wrote {
            step: Step::Guard,
            cpu,
            what: Wrote::PatchLevel,
            write: Write { at: 0, fault: Some(0), after: FIRMWARE, ..write },
            outcome: Outcome::Gp,
        },
        Reading::Wrote {
            step: Step::M2,
            cpu,
            what: Wrote::Loader(Staged { placement: Placement::Aligned, bytes: Bytes::LevelFlipped }),
            write: Write { fault: Some(0), after: FIRMWARE, ..write },
            outcome: Outcome::Gp,
        },
        Reading::Skipped { step: Step::M4, why: Why::NoSibling },
        Reading::Stopped { step: Step::M0, why: Why::AtOrAbove { cpu: 0, level: BENCH_LEVEL, patch: BENCH_LEVEL } },
        Reading::Done,
    ];
    let said: Vec<String> = lines.iter().map(|r| format!("{r}")).collect();
    assert_eq!(
        said,
        [
            "amducode: M0 cpu2 apic 4 core 1 level 0x8600106",
            "amducode: M5 cpu2 apic 4 core 1 wrmsr 0xc0010020 in-place intact at 0xffff80000012300c before 0x8600106 gp none after 0x860010d ticks 51234 ns 17078 outcome named",
            "amducode: guard cpu2 apic 4 core 1 wrmsr 0x8b 0 before 0x8600106 gp 0x0 after 0x8600106 ticks 51234 ns 17078 outcome gp",
            "amducode: M2 cpu2 apic 4 core 1 wrmsr 0xc0010020 aligned level-flipped at 0xffff80000012300c before 0x8600106 gp 0x0 after 0x8600106 ticks 51234 ns 17078 outcome gp",
            "amducode: M4 skipped: cpu0's core has no other thread online",
            "amducode: M0 stop: cpu0 runs 0x860010d, at or above the patch's 0x860010d, so AMD's published bytes cannot exercise a load",
            "amducode: done",
        ]
    );
}
