//! A GPT partition as a claimable device, judged off the disk.
//!
//! The guest (`tests/toyos-rust-tests/src/bin/partition_claimant.rs`) claims one
//! partition of a disk this file crafted, writes every block of it between the
//! kernel's own writes to a `/home` on the same disk, and asserts every
//! refusal the ABI promises. What it cannot judge is what its writes did to
//! the disk — that is the claim in question — so the verdict is read here,
//! after the guest has gone, off the image:
//!
//! - every byte outside the target and DATA is the byte this file wrote: the
//!   primary and backup tables, both neighbours, the granted partition and the
//!   gaps;
//! - both neighbours are FAT32 volumes `toyos-fat32-check` (fatgen103's rules)
//!   has nothing to say about — the neighbour after the target begins at the
//!   block after its last, so a write one past the end lands in its boot
//!   sector;
//! - every block of the target is the pattern the guest wrote there;
//! - the `/home` file written between the target's transfers reads back
//!   through the host's own bcachefs reader.
//!
//! The partition ranges are UEFI 2.10 §5.3.3's, as the `gpt` crate — not the
//! kernel's parser — laid them out.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use super::qemu::{self, BootOptions, QemuInstance};

/// Mirrored in the guest binary.
const TARGET_TYPE: &str = "7B1D4A3C-2E5F-4C8A-9D6B-0A1F2E3D4C5B";
const NEIGHBOUR_TYPE: &str = "5C3E8F21-9A4B-4D7E-8F10-2B3C4D5E6F70";
/// Mirrored in `tests/partclaimcase/system.toml` and the guest binary.
const GRANTED_TYPE: &str = "A94F0E6D-3B2C-4E1A-8C7D-6E5F4A3B2C1D";
const TARGET_BLOCKS: u64 = 2048;
const HOME_FILE: &str = "home/partclaim-interleaved.bin";
const HOME_CHUNK: usize = 32 * 1024;
const PAST_END: &[u8; 16] = b"TOYOS-PAST-END\0\0";
/// The claims the guest expects refused: four mounted partitions, the granted
/// one, an ambiguous type, an absent type, the zero GUID, and the target twice.
const REFUSALS: usize = 10;
/// The ESP, the log partition, ROOT and DATA.
const MOUNTED: usize = 4;
const BLOCK: u64 = 4096;
const MIB: u64 = 1024 * 1024;

/// The config whose one difference from the test estate is the granted row.
const CONFIG: &str = "tests/partclaimcase";

/// Mirrored in the guest: what block `n` of the target holds once it is done.
fn pattern(n: u64) -> Vec<u8> {
    let mut block = vec![0u8; BLOCK as usize];
    for (i, byte) in block.iter_mut().enumerate() {
        *byte = (n as usize).wrapping_mul(31).wrapping_add(i) as u8;
    }
    block[..8].copy_from_slice(&n.to_le_bytes());
    block[8..24].copy_from_slice(b"TOYOS-PARTCLAIM\0");
    block
}

/// Where one partition landed, in bytes.
#[derive(Clone, Copy, Debug)]
struct Span {
    start: u64,
    len: u64,
}

impl Span {
    fn end(self) -> u64 {
        self.start + self.len
    }
    fn of(self, disk: &[u8]) -> &[u8] {
        &disk[self.start as usize..self.end() as usize]
    }
}

struct Layout {
    before: Span,
    target: Span,
    after: Span,
    data: Span,
}

pub fn partition_claim(
    _test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let config = super::compile::repo_root().join(CONFIG);
    let image = super::lane::dir().join("partclaim-disk.img");
    let layout = craft(&image)?;

    // The premises, checked rather than assumed: a neighbour that did not
    // begin where the target ends would let a write past the end land in a
    // gap this test does not look at as hard.
    if layout.before.end() != layout.target.start || layout.target.end() != layout.after.start {
        return Err(format!("the neighbours do not touch the target: {:?}", (
            layout.before, layout.target, layout.after
        )));
    }
    if layout.target.len != TARGET_BLOCKS * BLOCK {
        return Err(format!("the target is {} bytes, not {TARGET_BLOCKS} blocks", layout.target.len));
    }
    let before = std::fs::read(&image).map_err(|e| format!("read the crafted disk: {e}"))?;
    for (what, span) in [("first", layout.before), ("second", layout.after)] {
        let complaints = toyos_fat32_check::check(span.of(&before));
        if !complaints.is_empty() {
            return Err(format!(
                "the {what} neighbour is not a clean FAT32 before any guest ran:\n{}",
                toyos_fat32_check::describe(&complaints)
            ));
        }
    }

    let mut qemu = QemuInstance::boot_with_options(
        &config,
        c_bins,
        rust_bins,
        BootOptions {
            profile: qemu::Profile::Metal,
            nvme_image: Some(image.clone()),
            ..Default::default()
        },
    );
    let boot = qemu.boot_log().to_string();
    for bad in ["PANIC:", "panicked at"] {
        if boot.contains(bad) {
            return Err(format!("{bad:?} booting the claim disk\n{boot}"));
        }
    }
    if boot.contains("are a tmpfs") {
        return Err(format!("/home fell back to tmpfs, so nothing shares the disk:\n{boot}"));
    }
    // init names what it could not mint; a grant it refused would make the
    // guest's `AlreadyExists` about nothing.
    let grant = format!("part-type:{GRANTED_TYPE}");
    if let Some(line) = boot.lines().find(|l| l.contains("init: test-runner:") && l.contains(&grant)) {
        return Err(format!("init did not grant test-runner its partition: {line}\n{boot}"));
    }

    // A guest that failed is judged off the disk all the same: what its failure
    // did to the neighbours is the half of the verdict it cannot give itself.
    let result = qemu.run_test("test_rs_partition_claimant", Duration::from_secs(180));
    let guest = guest_verdict(&result);
    writeln!(qemu.stdin_mut(), "run shutdown").expect("write to QEMU stdin");
    qemu.flush_stdin();
    let tail = qemu.drain_serial(Duration::from_secs(30));
    drop(qemu);
    for bad in ["PANIC:", "panicked at"] {
        if tail.contains(bad) {
            return Err(format!("{bad:?} on the way down\n{tail}"));
        }
    }

    let after = std::fs::read(&image).map_err(|e| format!("read the disk back: {e}"))?;
    let neighbours = neighbours_untouched(&layout, &before, &after);
    if guest.is_err() || !neighbours.is_empty() {
        return Err(format!(
            "guest: {}\nneighbours, off the image: {}",
            guest.err().unwrap_or_else(|| "every assertion held".to_string()),
            if neighbours.is_empty() { "untouched".to_string() } else { neighbours.join("\n") }
        ));
    }
    target_holds_the_pattern(&layout, &after)?;
    home_file_reads_back(&image)?;

    let _ = std::fs::remove_file(&image);
    eprintln!(
        "  [partclaim] {REFUSALS} claims refused by name; {TARGET_BLOCKS} blocks written and read \
         back through the claim; both FAT32 neighbours untouched byte for byte and clean to \
         fatgen103; /home intact"
    );
    Ok(())
}

/// What the guest said: exit 0, every refusal it asserted said by name — an
/// exit code alone is also what a binary that asserted nothing leaves — and the
/// kernel naming its own hold on each mounted partition it refused.
fn guest_verdict(result: &qemu::TestResult) -> Result<(), String> {
    if result.exit_code != Some(0) {
        return Err(format!(
            "the guest failed:\n{}\nkernel log while it ran:\n{}{}",
            result.stdout, result.before, result.serial
        ));
    }
    let refusals = result.stdout.lines().filter(|l| l.contains(" refused with ")).count();
    if refusals != REFUSALS || !result.stdout.contains("partition_claimant: PASS") {
        return Err(format!(
            "the guest exited 0 having said {refusals} of its {REFUSALS} refusals:\n{}",
            result.stdout
        ));
    }
    let kernel = format!("{}{}", result.before, result.serial);
    let held = kernel.matches("is held by the kernel").count();
    if held != MOUNTED {
        return Err(format!(
            "the kernel named its own hold {held} times for {MOUNTED} mounted partitions:\n{kernel}"
        ));
    }
    Ok(())
}

/// Everything that says a byte outside the target and DATA moved: the first
/// such byte, and each neighbour fatgen103's rules have something to say about.
fn neighbours_untouched(layout: &Layout, before: &[u8], after: &[u8]) -> Vec<String> {
    let mut found = Vec::new();
    if before.len() != after.len() {
        found.push(format!("the disk changed size: {} -> {}", before.len(), after.len()));
        return found;
    }
    let owned = [layout.target, layout.data];
    let mut at = 0u64;
    while at < before.len() as u64 {
        if let Some(span) = owned.iter().find(|s| s.start <= at && at < s.end()) {
            at = span.end();
            continue;
        }
        let i = at as usize;
        if before[i] != after[i] {
            let block = at / BLOCK;
            let past_end = after[(block * BLOCK) as usize..][..PAST_END.len()] == *PAST_END;
            found.push(format!(
                "byte {at} (device block {block}) changed outside the claimed partition{}",
                if past_end { " — it holds the write the guest made past the end" } else { "" }
            ));
            break;
        }
        at += 1;
    }
    for (what, span) in [("first", layout.before), ("second", layout.after)] {
        let complaints = toyos_fat32_check::check(span.of(after));
        if !complaints.is_empty() {
            found.push(format!(
                "the {what} neighbour is not the FAT32 it was:\n{}",
                toyos_fat32_check::describe(&complaints)
            ));
        }
    }
    found
}

/// Every block of the target is the pattern the guest wrote there.
fn target_holds_the_pattern(layout: &Layout, after: &[u8]) -> Result<(), String> {
    let target = layout.target.of(after);
    for n in 0..TARGET_BLOCKS {
        let got = &target[(n * BLOCK) as usize..((n + 1) * BLOCK) as usize];
        if got != pattern(n) {
            return Err(format!("target block {n} is not what the guest wrote there"));
        }
    }
    Ok(())
}

/// The `/home` file the guest wrote between the target's transfers, through
/// the host's bcachefs reader over a plain seek-and-read of the image.
fn home_file_reads_back(image: &Path) -> Result<(), String> {
    let io = super::storage::FileBlocks::open(image)?;
    let fs = bcachefs::Mounted::<_, bcachefs::ReadOnly>::open(io)
        .map_err(|e| format!("DATA does not mount on the host: {e:?}"))?;
    let got = fs.read_file(HOME_FILE).map_err(|e| format!("reading {HOME_FILE}: {e:?}"))?;
    let runs = TARGET_BLOCKS.div_ceil(32);
    let want: Vec<u8> = (0..runs)
        .filter(|run| run % 8 == 0)
        .flat_map(|run| (0..HOME_CHUNK).map(move |i| (run as usize ^ i) as u8))
        .collect();
    if got != want {
        return Err(format!(
            "{HOME_FILE} is {} bytes off the image against the {} the guest wrote",
            got.len(),
            want.len()
        ));
    }
    Ok(())
}

/// The disk: a FAT32 neighbour, the target, a FAT32 neighbour touching it, the
/// partition init grants, and a DATA the kernel formats and mounts at `/home`.
fn craft(path: &Path) -> Result<Layout, String> {
    const FAT_BYTES: u64 = 34 * MIB;
    const GRANTED_BYTES: u64 = MIB;
    const DATA_BYTES: u64 = 96 * MIB;
    let total = MIB + 2 * FAT_BYTES + TARGET_BLOCKS * BLOCK + GRANTED_BYTES + DATA_BYTES + 2 * MIB;

    let file = std::fs::File::create(path).map_err(|e| format!("create the disk: {e}"))?;
    file.set_len(total).map_err(|e| format!("size the disk: {e}"))?;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| format!("open the disk: {e}"))?;
    let mbr = gpt::mbr::ProtectiveMBR::with_lb_size(
        u32::try_from(total / 512 - 1).unwrap_or(0xFFFF_FFFF),
    );
    mbr.overwrite_lba0(&mut file).map_err(|e| format!("protective MBR: {e}"))?;
    let mut gdisk = gpt::GptConfig::default()
        .initialized(false)
        .writable(true)
        .logical_block_size(gpt::disk::LogicalBlockSize::Lb512)
        .create_from_device(Box::new(file), None)
        .map_err(|e| format!("create the table: {e}"))?;
    gdisk
        .update_partitions(std::collections::BTreeMap::new())
        .map_err(|e| format!("initialise the table: {e}"))?;

    let ty = |guid: &'static str| gpt::partition_types::Type {
        guid,
        os: gpt::partition_types::OperatingSystem::None,
    };
    let align = Some(MIB / 512);
    let mut add = |name: &str, bytes: u64, guid: &'static str| {
        gdisk.add_partition(name, bytes, ty(guid), 0, align).map_err(|e| format!("add {name}: {e}"))
    };
    let ids = [
        add("neighbour before", FAT_BYTES, NEIGHBOUR_TYPE)?,
        add("claim target", TARGET_BLOCKS * BLOCK, TARGET_TYPE)?,
        add("neighbour after", FAT_BYTES, NEIGHBOUR_TYPE)?,
        add("granted", GRANTED_BYTES, GRANTED_TYPE)?,
        add("ToyOS data", DATA_BYTES, toyos_gpt::Guid::TOYOS_DATA_TEXT)?,
    ];
    let span = |id: u32| -> Result<Span, String> {
        let part = gdisk.partitions().get(&id).ok_or("a partition that was just added")?;
        let lb = gpt::disk::LogicalBlockSize::Lb512;
        Ok(Span {
            start: part.bytes_start(lb).map_err(|e| format!("start: {e}"))?,
            len: part.bytes_len(lb).map_err(|e| format!("length: {e}"))?,
        })
    };
    let layout = Layout {
        before: span(ids[0])?,
        target: span(ids[1])?,
        after: span(ids[2])?,
        data: span(ids[4])?,
    };
    let mut device = gdisk.write().map_err(|e| format!("write the table: {e}"))?;

    use std::io::{Seek, SeekFrom};
    for (label, span) in [("PC-BEFORE", layout.before), ("PC-AFTER", layout.after)] {
        let volume = fat32(span.len as usize, label)?;
        device.seek(SeekFrom::Start(span.start)).map_err(|e| format!("seek: {e}"))?;
        device.write_all(&volume).map_err(|e| format!("write {label}: {e}"))?;
    }
    // The designation: the kernel formats a DATA only on this consent.
    let mut stamp = [0u8; BLOCK as usize];
    stamp[..bcachefs::DESIGNATION_MAGIC.len()].copy_from_slice(&bcachefs::DESIGNATION_MAGIC);
    let at = bcachefs::DESIGNATION_BLOCKS_OFFSET;
    stamp[at..at + 8].copy_from_slice(&(layout.data.len / BLOCK).to_le_bytes());
    device.seek(SeekFrom::Start(layout.data.start)).map_err(|e| format!("seek: {e}"))?;
    device.write_all(&stamp).map_err(|e| format!("stamp DATA: {e}"))?;
    device.flush().map_err(|e| format!("flush the disk: {e}"))?;
    Ok(layout)
}

/// A FAT32 volume of `bytes` holding one file, so the check has a directory
/// entry and a cluster chain to judge and not only a boot sector.
fn fat32(bytes: usize, label: &str) -> Result<Vec<u8>, String> {
    let mut volume = vec![0u8; bytes];
    let mut name = [b' '; 11];
    name[..label.len()].copy_from_slice(label.as_bytes());
    fatfs::format_volume(
        std::io::Cursor::new(&mut volume),
        fatfs::FormatVolumeOptions::new().fat_type(fatfs::FatType::Fat32).volume_label(name),
    )
    .map_err(|e| format!("format {label}: {e}"))?;
    {
        let fs = fatfs::FileSystem::new(std::io::Cursor::new(&mut volume), fatfs::FsOptions::new())
            .map_err(|e| format!("mount {label} on the host: {e}"))?;
        let mut file = fs.root_dir().create_file("NEIGHBOUR.TXT").map_err(|e| format!("{e}"))?;
        file.write_all(label.as_bytes()).map_err(|e| format!("{e}"))?;
        file.flush().map_err(|e| format!("{e}"))?;
        drop(file);
        // Counted before the unmount so FSInfo carries a free count, as every
        // writer that has finished with a volume leaves it.
        fs.stats().map_err(|e| format!("count {label}'s free clusters: {e}"))?;
        fs.unmount().map_err(|e| format!("unmount {label}: {e}"))?;
    }
    Ok(volume)
}
