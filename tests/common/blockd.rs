//! blockd, the NVMe driver in userland, judged off the disk it wrote and off
//! the device's own trace.
//!
//! The guest (`tests/toyos-rust-tests/src/bin/blockd_io.rs`) is blockd's
//! supervisor and client both. What it cannot judge about itself is what
//! reached the medium and what the controller was actually sent, so both are
//! read here after the guest is gone: the image, with the host's own readers —
//! `toyos-fat32-check` (fatgen103's rules) and the `fatfs` crate, neither of
//! which is the code that wrote the volume — and QEMU's trace of every NVMe
//! command, completion and flush, which no driver can print on the device's
//! behalf.
//!
//! The machine has two NVMe controllers: the kernel's (QEMU's own ids, first
//! by enumeration, so the kernel's first-by-class probe takes it) with DATA and
//! a bench partition, and blockd's (Intel's ids) with the partitions below.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::partclaim::{self, Part, Span, ALIGNED, NEIGHBOUR_TYPE, PLAIN_TYPE};
use super::qemu::{BootOptions, QemuInstance, TestResult};
use super::serial::Serial;

/// Mirrored in the guest: blockd's disk.
const TARGET: &str = "9C4E2A71-5B3D-4F18-A6E0-2D7C8B1F3E59";
const FS: &str = "B2D4F6A8-1C3E-4A57-9B0D-E2F4A6C8E0A1";
const BENCH: &str = "C3E5A7B9-2D4F-4B68-8C1E-F3A5B7D9F1B2";
const MISALIGNED: &str = "E5A7C9DB-4F6B-4D8A-8E30-B5C7D9FB13D4";
const MISSTART: &str = "F6B8DAEC-5A7C-4E9B-9F41-C6D8EA0C24E5";
/// Mirrored: the kernel's disk.
const KBENCH: &str = "D4F6B8CA-3E5A-4C79-9D2F-A4B6C8EA02C3";
const TARGET_BLOCKS: u64 = 2048;
const BENCH_BLOCKS: u64 = 8192;
const FILES: usize = 6;
const FILE_BYTES: usize = 48 * 1024;
const AFTER: &str = "AFTER.BIN";

const BLOCK: u64 = 4096;
const MIB: u64 = 1024 * 1024;
const CONFIG: &str = "tests/blockdcase";

/// Mirrored: block `n` of a region `salt` names.
fn pattern(salt: u8, n: u64) -> Vec<u8> {
    let mut block = vec![0u8; BLOCK as usize];
    for (i, byte) in block.iter_mut().enumerate() {
        *byte = (n as usize).wrapping_mul(131).wrapping_add(i).wrapping_add(salt as usize) as u8;
    }
    block[..8].copy_from_slice(&n.to_le_bytes());
    block[8] = salt;
    block[9..24].copy_from_slice(b"TOYOS-BLOCKDIO\0");
    block
}

/// Mirrored: file `i`'s bytes.
fn file_bytes(i: usize) -> Vec<u8> {
    (0..FILE_BYTES).map(|b| (b.wrapping_mul(7) ^ i.wrapping_mul(0x3D)) as u8).collect()
}

/// Where blockd's partitions landed.
struct Layout {
    before: Span,
    target: Span,
    after: Span,
    fs: Span,
    bench: Span,
}

/// blockd's disk: a FAT32 neighbour, the idle ROOT slot, a FAT32 neighbour
/// touching it, the FAT32 volume the crash role writes, the bench partition,
/// and two partitions that are not whole 4 KiB blocks.
fn craft_blockd_disk(path: &Path) -> Result<Layout, String> {
    const NEIGHBOUR_BYTES: u64 = 34 * MIB;
    const FS_BYTES: u64 = 64 * MIB;
    let parts: [Part; 7] = [
        ("neighbour before", NEIGHBOUR_BYTES, NEIGHBOUR_TYPE, "21111111-2222-4333-8444-555555555501", ALIGNED),
        ("idle ROOT slot", TARGET_BLOCKS * BLOCK, toyos_gpt::Guid::TOYOS_ROOT_TEXT, TARGET, ALIGNED),
        ("neighbour after", NEIGHBOUR_BYTES, NEIGHBOUR_TYPE, "21111111-2222-4333-8444-555555555502", ALIGNED),
        ("fs", FS_BYTES, PLAIN_TYPE, FS, ALIGNED),
        ("bench", BENCH_BLOCKS * BLOCK, PLAIN_TYPE, BENCH, ALIGNED),
        ("misaligned", MIB + 512, PLAIN_TYPE, MISALIGNED, ALIGNED),
        // At the first LBA past the one above: whole blocks long, and
        // beginning 512 bytes into one.
        ("misaligned start", MIB, PLAIN_TYPE, MISSTART, 1),
    ];
    let total = MIB + parts.iter().map(|p| p.1.next_multiple_of(MIB)).sum::<u64>() + 2 * MIB;
    let (mut device, spans) = partclaim::table(path, total, &parts)?;
    let layout = Layout { before: spans[0], target: spans[1], after: spans[2], fs: spans[3], bench: spans[4] };
    for (label, span) in [("BD-BEFORE", layout.before), ("BD-AFTER", layout.after), ("BD-FS", layout.fs)] {
        let volume = partclaim::fat32(span.len as usize, label)?;
        device.seek(SeekFrom::Start(span.start)).map_err(|e| format!("seek: {e}"))?;
        device.write_all(&volume).map_err(|e| format!("write {label}: {e}"))?;
    }
    device.flush().map_err(|e| format!("flush the disk: {e}"))?;
    Ok(layout)
}

/// A boot with both disks crafted fresh, and QEMU tracing NVMe to `trace`.
fn boot(
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
    name: &str,
) -> Result<(QemuInstance, Layout, PathBuf, PathBuf, Vec<u8>), String> {
    let config = super::compile::repo_root().join(CONFIG);
    let kernel_disk = super::lane::dir().join(format!("{name}-kernel.img"));
    partclaim::craft_plain_disk(&kernel_disk, &[("kernel bench", BENCH_BLOCKS * BLOCK, KBENCH)], 96 * MIB)?;
    let blockd_disk = super::lane::dir().join(format!("{name}-blockd.img"));
    let layout = craft_blockd_disk(&blockd_disk)?;
    let before = std::fs::read(&blockd_disk).map_err(|e| format!("read the crafted disk: {e}"))?;
    let trace = super::lane::dir().join(format!("{name}-nvme.trace"));
    let _ = std::fs::remove_file(&trace);
    let qemu = QemuInstance::boot_with_options(
        &config,
        c_bins,
        rust_bins,
        BootOptions {
            nvme_image: Some(kernel_disk),
            userland_nvme: Some(blockd_disk.clone()),
            nvme_trace: Some(trace.clone()),
            ..Default::default()
        },
    );
    partclaim::no_panic("booting", qemu.boot_log())?;
    Ok((qemu, layout, blockd_disk, trace, before))
}

/// One role of the guest, which must end `PASS <role>` and exit 0.
fn role(qemu: &mut QemuInstance, role: &str, timeout: Duration) -> Result<TestResult, String> {
    let result = qemu.run_test(&format!("test_rs_blockd_io {role}"), timeout);
    if result.exit_code != Some(0) || !result.stdout.contains(&format!("blockd_io: PASS {role}")) {
        return Err(format!(
            "role {role} exited {:?}:\n{}\nkernel log while it ran:\n{}{}",
            result.exit_code, result.stdout, result.before, result.serial
        ));
    }
    Ok(result)
}

fn said<'a>(result: &'a TestResult, needle: &str) -> Result<&'a str, String> {
    result
        .stdout
        .lines()
        .find(|l| l.contains(needle))
        .ok_or_else(|| format!("the guest never said {needle:?}:\n{}", result.stdout))
}

/// What QEMU's trace says reached blockd's controller.
struct Traced {
    /// Flush commands the device ran.
    flushes: usize,
    /// Submission queues reads and writes arrived on.
    queues: BTreeSet<u16>,
    /// The most commands outstanding at once on queues 2 and up — the
    /// kernel's driver has one I/O queue, so those are blockd's alone.
    peak: usize,
}

/// A trace line's `key value` field.
fn field(line: &str, key: &str) -> Option<u64> {
    let mut words = line.split_whitespace();
    while let Some(word) = words.next() {
        if word == key {
            let value = words.next()?;
            return match value.strip_prefix("0x") {
                Some(hex) => u64::from_str_radix(hex, 16).ok(),
                None => value.parse().ok(),
            };
        }
    }
    None
}

fn read_trace(trace: &Path) -> Result<Traced, String> {
    let text = std::fs::read_to_string(trace).map_err(|e| format!("read the NVMe trace: {e}"))?;
    let mut flushes = 0;
    let mut queues = BTreeSet::new();
    let mut open: BTreeSet<(u64, u64)> = BTreeSet::new();
    let mut peak = 0;
    for line in text.lines() {
        if line.contains("pci_nvme_flush_ns") {
            flushes += 1;
        } else if line.contains("pci_nvme_io_cmd") {
            let (Some(cid), Some(sqid), Some(opc)) = (field(line, "cid"), field(line, "sqid"), field(line, "opc"))
            else {
                return Err(format!("a trace line this reader does not know: {line:?}"));
            };
            if opc == 1 || opc == 2 {
                queues.insert(sqid as u16);
            }
            if sqid >= 2 {
                open.insert((sqid, cid));
                peak = peak.max(open.len());
            }
        } else if line.contains("pci_nvme_enqueue_req_completion") {
            let (Some(cid), Some(cqid)) = (field(line, "cid"), field(line, "cqid")) else {
                return Err(format!("a trace line this reader does not know: {line:?}"));
            };
            open.remove(&(cqid, cid));
        }
    }
    Ok(Traced { flushes, queues, peak })
}

/// Every byte outside the partitions the guest may write is the byte the host
/// wrote, and both FAT32 neighbours are clean to fatgen103.
fn neighbours_untouched(layout: &Layout, before: &[u8], after: &[u8]) -> Result<(), String> {
    if before.len() != after.len() {
        return Err(format!("the disk changed size: {} -> {}", before.len(), after.len()));
    }
    let owned = [layout.target, layout.fs, layout.bench];
    let mut at = 0u64;
    while at < before.len() as u64 {
        if let Some(span) = owned.iter().find(|s| s.start <= at && at < s.end()) {
            at = span.end();
            continue;
        }
        if before[at as usize] != after[at as usize] {
            return Err(format!("byte {at} (block {}) changed outside every partition a session held", at / BLOCK));
        }
        at += 1;
    }
    for (what, span) in [("first", layout.before), ("second", layout.after)] {
        let complaints = toyos_fat32_check::check(span.of(after));
        if !complaints.is_empty() {
            return Err(format!(
                "the {what} neighbour is not the FAT32 it was:\n{}",
                toyos_fat32_check::describe(&complaints)
            ));
        }
    }
    Ok(())
}

/// The partition claims served by blockd, and blockd against the kernel's
/// driver.
///
/// - Every refusal by name, the idle ROOT slot's 2048 blocks written whole
///   through a session and read back, and one holder at a time across two
///   processes — the slot a second client is refused while it is held and
///   opened once it is not.
/// - The same bytes through the kernel's driver and through blockd, timed.
/// - Off the image: the slot holds every block the guest wrote, and nothing
///   outside the sessions' partitions moved.
/// - Off QEMU's trace, which no driver writes: blockd's Flush reached the
///   device, reads and writes went down several submission queues, and more
///   than one command was outstanding at once.
pub fn blockd_serves_partitions(
    _test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let (mut qemu, layout, disk, trace, before) = boot(c_bins, rust_bins, "blockd-serves")?;
    let claims = role(&mut qemu, "claims", Duration::from_secs(240))?;
    for want in [
        "an absent GUID refused with NotFound",
        "the zero GUID refused with NotFound",
        "a partition not whole blocks long refused with Unusable",
        "a partition beginning inside a block refused with Unusable",
        "a second client of the slot refused with Held",
        "a second client of the slot refused with Opened",
        "blockd: NVMe up:",
    ] {
        said(&claims, want)?;
    }
    let cache = said(&claims, "volatile write cache")?.to_string();
    if !cache.contains("present, so a flush issues Flush") {
        return Err(format!("blockd's controller reports no volatile write cache: {cache}"));
    }
    let bench = role(&mut qemu, "bench", Duration::from_secs(600))?;
    let numbers = said(&bench, "blockd_io: bench")?.to_string();
    let tail = partclaim::shut_down(qemu);
    partclaim::no_panic("on the way down", &tail)?;
    let mut log = Serial::named("blockd_serves_partitions", format!("{}{}", claims.serial, bench.serial));
    log.push(&tail);
    log.must_be_clean()?;

    let after = std::fs::read(&disk).map_err(|e| format!("read the disk back: {e}"))?;
    neighbours_untouched(&layout, &before, &after)?;
    let slot = layout.target.of(&after);
    for n in 0..TARGET_BLOCKS {
        if slot[(n * BLOCK) as usize..((n + 1) * BLOCK) as usize] != pattern(0x5A, n)[..] {
            return Err(format!("slot block {n} is not what the guest wrote there"));
        }
    }
    let traced = read_trace(&trace)?;
    if traced.flushes == 0 {
        return Err("QEMU ran no Flush, and blockd's flushes all said durable".into());
    }
    if traced.queues.len() < 2 || traced.peak < 2 {
        return Err(format!(
            "QEMU saw reads and writes on submission queues {:?} and at most {} of blockd's commands \
             outstanding at once",
            traced.queues, traced.peak
        ));
    }
    let _ = std::fs::remove_file(&disk);
    let _ = std::fs::remove_file(&trace);
    eprintln!(
        "  [blockd] {numbers}; QEMU traced {} Flush commands, reads and writes on submission queues \
         {:?}, and at most {} of blockd's commands outstanding at once; the idle slot holds all \
         {TARGET_BLOCKS} blocks and nothing outside the sessions' partitions moved",
        traced.flushes, traced.queues, traced.peak
    );
    Ok(())
}

/// blockd's two failures, each survived by its client.
///
/// - `reset`: blockd withholds its second write's answer; the silence ends in
///   a controller reset; the withheld write is answered not done; and the
///   write acknowledged before it, which the reset may have lost, is on the
///   medium after the next flush — blockd says the flush found the loss.
/// - `crash`: a FAT32 volume written through a session, blockd killed the
///   moment it has done a write it never answered, with two acknowledged and
///   not flushed; restarted on the same port, the session reopened, the same
///   mount carried on. Off the image, with the host's own readers: the volume
///   is clean to fatgen103, and every file the guest was told was written —
///   and the one written after the restart — holds its bytes by `fatfs`.
pub fn blockd_survives_its_death(
    _test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let (mut qemu, layout, disk, trace, before) = boot(c_bins, rust_bins, "blockd-death")?;
    let reset = role(&mut qemu, "reset", Duration::from_secs(240))?;
    for want in [
        "blockd: WITHHELD the device's answer to a write",
        "resetting the controller",
        "blockd: controller reset;",
        "a flush found writes of its own the device lost",
        "the withheld write was answered Device",
        "1 acknowledged writes no flush had covered went out again after the reset",
    ] {
        said(&reset, want)?;
    }
    let crash = role(&mut qemu, "crash", Duration::from_secs(600))?;
    for want in [
        "blockd: WITHHELD the device's answer to a write",
        "blockd killed with the withheld write done on the device",
        "was answered Refused",
        "blockd restarted and the session reopened",
        "2 acknowledged writes no flush had covered went out again after the restart",
    ] {
        said(&crash, want)?;
    }
    let tail = partclaim::shut_down(qemu);
    partclaim::no_panic("on the way down", &tail)?;
    let mut log = Serial::named("blockd_survives_its_death", format!("{}{}", reset.serial, crash.serial));
    log.push(&tail);
    log.must_be_clean()?;

    let after = std::fs::read(&disk).map_err(|e| format!("read the disk back: {e}"))?;
    neighbours_untouched(&layout, &before, &after)?;
    let volume = layout.fs.of(&after);
    let complaints = toyos_fat32_check::check(volume);
    if !complaints.is_empty() {
        return Err(format!(
            "the volume blockd died under is not clean:\n{}",
            toyos_fat32_check::describe(&complaints)
        ));
    }
    let mut image = volume.to_vec();
    let fs = fatfs::FileSystem::new(std::io::Cursor::new(&mut image), fatfs::FsOptions::new())
        .map_err(|e| format!("the volume does not mount on the host: {e}"))?;
    let root = fs.root_dir();
    let mut files: BTreeMap<String, Vec<u8>> =
        (0..FILES).map(|i| (format!("F{i}.BIN"), file_bytes(i))).collect();
    files.insert(AFTER.to_string(), file_bytes(99));
    for (name, want) in &files {
        let mut got = Vec::new();
        root.open_file(name)
            .map_err(|e| format!("{name} is not on the volume: {e}"))?
            .read_to_end(&mut got)
            .map_err(|e| format!("{name}: {e}"))?;
        if &got != want {
            return Err(format!("{name} is {} bytes of something else off the image", got.len()));
        }
    }
    drop(root);
    drop(fs);
    let _ = std::fs::remove_file(&disk);
    let _ = std::fs::remove_file(&trace);
    eprintln!(
        "  [blockd] a controller reset under a withheld write: answered not done, and the write \
         before it rewritten after the flush found it lost; blockd killed with a write done and \
         unanswered: restarted, the session reopened, the mount carried on, and off the image the \
         volume is clean to fatgen103 and all {} files read back by fatfs",
        files.len()
    );
    Ok(())
}

/// A transfer outside what a claim's function was lent is the unit's fault
/// record and nothing else.
///
/// The guest drives blockd's controller itself (`blockd::nvme`), lending it one
/// region with `SYS_DEVICE_DMA_MAP`, four times, each on a fresh claim:
/// - a read into the lent region lands there, and the function's own register
///   window and a region already lent are refused as regions to lend;
/// - a read aimed at the first address past it is refused at the unit, and
///   the region is untouched;
/// - a read aimed at the region after `SYS_DEVICE_DMA_UNMAP` took it back is
///   refused at the unit, and the region — still the guest's — is untouched;
/// - and the function, released and claimed again, reads into a lent region
///   as the first did.
///
/// The oracle is the unit: each refusal is one `DMA FAULT` record the kernel
/// wrote from the fault recording registers (VT-d 3.0 §7.2), at the address
/// the guest aimed at, blamed on the claim's slot, with a second-level
/// reason — and nothing on the machine died.
pub fn blockd_dma_outside_the_lent(
    _test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let (mut qemu, _layout, disk, trace, _before) = boot(c_bins, rust_bins, "blockd-dma")?;
    let mut log = String::new();
    let mut aimed = Vec::new();
    for name in ["dma-inside", "dma-outside", "dma-revoked", "dma-after"] {
        let result = role(&mut qemu, name, Duration::from_secs(120))?;
        if name == "dma-inside" {
            said(&result, "a register window, and a region already lent, are refused with InvalidArgument")?;
        }
        if let Some(line) = result.stdout.lines().find(|l| l.contains("aiming the device at ")) {
            let at = line
                .split("aiming the device at ")
                .nth(1)
                .and_then(|rest| rest.split([',', ' ']).next())
                .ok_or_else(|| format!("no address in {line:?}"))?;
            aimed.push((name, at.to_string()));
        }
        log.push_str(&result.before);
        log.push_str(&result.serial);
    }
    let tail = partclaim::shut_down(qemu);
    partclaim::no_panic("on the way down", &tail)?;
    log.push_str(&tail);
    let log = Serial::named("blockd_dma_outside_the_lent", log);
    log.must_be_clean_apart_from("iommu: DMA FAULT owner=slot", 2)?;
    for (name, at) in &aimed {
        let want = format!("addr={:#018x}", u64::from_str_radix(at.trim_start_matches("0x"), 16).map_err(|e| format!("{at}: {e}"))?);
        let line = log
            .text()
            .lines()
            .find(|l| l.contains("iommu: DMA FAULT owner=slot") && l.contains(&want))
            .ok_or_else(|| format!("{name}: no fault record at {want}:\n{}", log.text()))?;
        if !["read-permission", "write-permission", "paging-entry-invalid"].iter().any(|r| line.ends_with(r)) {
            return Err(format!("{name}: the record's reason is not a second-level walk's: {line}"));
        }
        eprintln!("  [blockd] {name}: {}", line.trim());
    }
    if aimed.len() != 2 {
        return Err(format!("the guest aimed outside the lent region {} times, not 2", aimed.len()));
    }
    let _ = std::fs::remove_file(&disk);
    let _ = std::fs::remove_file(&trace);
    eprintln!(
        "  [blockd] a read into a lent region landed; one aimed past it and one at it after it was \
         taken back were each one fault record at that address and left the region untouched; the \
         function answered again on its next claim"
    );
    Ok(())
}
