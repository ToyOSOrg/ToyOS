//! The interlock that keeps ToyOS off a disk it was not given.
//!
//! The claim under test is not "formatting works" -- `nvme_large_device` has
//! that -- but its negative: **a device the kernel was not given comes back
//! byte-for-byte unchanged.** That is asserted against the backing file, on
//! the host, because the guest's account of what it did to a disk is exactly
//! the thing in question. The stimulus is a disk that holds something, mounts
//! as nothing, and belongs to someone -- which a kernel reading "mount returned
//! None" as permission to format would take.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use toyos_build::fingerprint::{first_difference, whole_device};

use super::qemu::{self, BootOptions, QemuInstance};

/// Boot the guest against a disk that belongs to somebody else, and prove it
/// comes back untouched.
///
/// Lives here so the registration hunk in `toyos.rs` stays one line: every
/// agent edits that file.
pub fn foreign_disk_untouched(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    const BYTES: u64 = 128 * 1024 * 1024;
    // The same directory `boot_with_options` uses, named here because this
    // image has to exist before the boot that must not touch it.
    let dir = super::lane::dir();
    let image = dir.join("foreign-disk.img");
    let (data_at, _) = foreign_disk_image(&image, BYTES);
    let before = whole_device(&image);

    // The premise, checked before the boot rather than assumed: if this volume
    // somehow already parsed as a ToyOS volume, the kernel would mount it and
    // the assertion below would pass for the wrong reason.
    if front(&image, data_at, 4) == *b"BCFS" {
        return Err("the foreign volume starts with a bcachefs superblock".to_string());
    }

    let mut qemu = QemuInstance::boot_with_options(
        test_config,
        c_bins,
        rust_bins,
        BootOptions {
            profile: qemu::Profile::Metal,
            nvme_image: Some(image.clone()),
            ..Default::default()
        },
    );

    // The boot log, not a post-ready drain: every line this test cares about
    // is printed in the storage phase, long before the ready marker.
    let log = qemu.boot_log().to_string();
    for bad in ["PANIC:", "panicked at"] {
        if log.contains(bad) {
            return Err(format!("{bad:?}: refusing a disk must not be fatal\n{log}"));
        }
    }
    // The refusal is stated, not inferred. A kernel that never reached the
    // storage phase would also leave the image untouched.
    const REFUSED: &str = "this disk is not ours";
    if !log.contains(REFUSED) {
        return Err(format!("the kernel never said {REFUSED:?} — did it reach storage?\n{log}"));
    }
    // And the machine still came up, because a refusal that costs the boot is
    // a refusal nobody will leave switched on.
    if !log.contains("Boot: complete") {
        return Err(format!("the boot did not complete on a disk it refused\n{log}"));
    }
    // Independent of anything that reaches the platter, and deliberately so:
    // the byte comparison below can only see writes that were flushed, and a
    // format that is still sitting in the page cache has already destroyed the
    // disk as far as the next sync is concerned.
    if log.contains("formatting it") {
        return Err(format!("the kernel decided to format a disk it was not given\n{log}"));
    }

    // Shut down rather than kill: `PageCache::sync` at shutdown is the only
    // thing that moves a format from the cache to the device, so a killed QEMU
    // fingerprints an image a formatting kernel would also have left untouched.
    writeln!(qemu.stdin_mut(), "run shutdown").expect("write to QEMU stdin");
    qemu.flush_stdin();
    let tail = qemu.drain_serial(Duration::from_secs(20));
    for bad in ["PANIC:", "panicked at"] {
        if tail.contains(bad) {
            return Err(format!("{bad:?} during shutdown\n{tail}"));
        }
    }
    drop(qemu);

    let after = whole_device(&image);
    if let Some(diff) = first_difference(&before, &after) {
        return Err(format!("the kernel wrote to a disk it was not given: {diff}"));
    }
    let _ = std::fs::remove_file(&image);
    Ok(())
}

/// The volume is genuine and the disk is not: block 0 here carries the magic,
/// the version and the CRC this crate wrote, and every other stimulus in this
/// file is refused before any of that is read. What it does not carry is this
/// device's block count, and a read-write mount writes on sight.
pub fn volume_from_another_disk(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    const VOLUME_BLOCKS: u64 = 4096;
    const DEVICE_BYTES: u64 = 128 * 1024 * 1024;
    let dir = super::lane::dir();
    let image = dir.join("copied-volume.img");

    let mut fs = bcachefs::Formatted::format(bcachefs::VecBlockIO::new(VOLUME_BLOCKS))
        .map_err(|e| format!("format a volume on the host: {e:?}"))?;
    fs.create("stranger.txt", b"a file that was already here", 1)
        .map_err(|e| format!("put a file on the host volume: {e:?}"))?;
    let volume = fs
        .into_io()
        .map_err(|e| format!("sync the host volume: {e:?}"))?
        .into_vec();

    // The premise, checked before the boot: the guest's refusal below is about
    // the device's size and not about an image nothing could have mounted.
    bcachefs::Mounted::<_, bcachefs::ReadOnly>::open(bcachefs::VecBlockIO::from_vec(
        volume.clone(),
    ))
    .map_err(|e| format!("the volume this test wrote does not mount on its own device: {e:?}"))?;

    // On a device the volume was not formatted for, inside a partition it was
    // not formatted for: the copy lands over the designation stamp, so the
    // kernel finds a real superblock naming a block count that is not this
    // partition's.
    let file = std::fs::File::create(&image).map_err(|e| format!("create the image: {e}"))?;
    file.set_len(DEVICE_BYTES).map_err(|e| format!("grow the device under the volume: {e}"))?;
    let (at, _) = toyos_build::image::designate_data_disk(&image, DEVICE_BYTES);
    {
        use std::io::{Seek, SeekFrom, Write};
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(&image)
            .map_err(|e| format!("open the image: {e}"))?;
        file.seek(SeekFrom::Start(at)).map_err(|e| format!("seek: {e}"))?;
        file.write_all(&volume).map_err(|e| format!("write the copied volume: {e}"))?;
    }
    let before = whole_device(&image);

    let mut qemu = QemuInstance::boot_with_options(
        test_config,
        c_bins,
        rust_bins,
        BootOptions {
            profile: qemu::Profile::Metal,
            nvme_image: Some(image.clone()),
            ..Default::default()
        },
    );
    let log = qemu.boot_log().to_string();
    for bad in ["PANIC:", "panicked at"] {
        if log.contains(bad) {
            return Err(format!("{bad:?}: refusing a copied volume must not be fatal\n{log}"));
        }
    }
    const MOUNTED: &str = "mounted the ToyOS volume at block 0";
    if log.contains(MOUNTED) {
        return Err(format!(
            "the kernel mounted a volume that did not come from this disk: it said \
             {MOUNTED:?}\n{log}"
        ));
    }
    const REFUSED: &str = "this disk is not ours";
    if !log.contains(REFUSED) {
        return Err(format!("the kernel never said {REFUSED:?} — did it reach storage?\n{log}"));
    }
    if log.contains("formatting it") {
        return Err(format!("the kernel decided to format a disk it was not given\n{log}"));
    }
    if !log.contains("Boot: complete") {
        return Err(format!("the boot did not complete on a volume it refused\n{log}"));
    }

    // Down through `PageCache::sync`, the only thing that moves a write out of
    // the cache and onto the device.
    writeln!(qemu.stdin_mut(), "run shutdown").expect("write to QEMU stdin");
    qemu.flush_stdin();
    let tail = qemu.drain_serial(Duration::from_secs(20));
    for bad in ["PANIC:", "panicked at"] {
        if tail.contains(bad) {
            return Err(format!("{bad:?} during shutdown\n{tail}"));
        }
    }
    drop(qemu);

    let after = whole_device(&image);
    if let Some(diff) = first_difference(&before, &after) {
        return Err(format!("the kernel wrote to a volume it refused: {diff}"));
    }
    let _ = std::fs::remove_file(&image);
    Ok(())
}

/// A disk carrying a TOYOS-DATA partition that is somebody else's, and where
/// it landed.
///
/// **The partition is ToyOS-typed on purpose**: a disk with no such partition
/// is refused before block 0 is read and could not exercise the probe at all.
/// Here the kernel finds the candidate, opens the view, reads block 0, and has
/// to refuse it there — the volume holding neither a bcachefs superblock nor a
/// designation stamp is the only property that matters.
pub fn foreign_disk_image(path: &Path, len: u64) -> (u64, u64) {
    use std::io::{Seek, SeekFrom, Write};

    let file = std::fs::File::create(path).expect("create foreign image");
    file.set_len(len).expect("size foreign image");
    let (at, bytes) = toyos_build::image::designate_data_disk(path, len);

    // Over the stamp the writer left: consent is what this disk must not carry.
    let mut volume = [0u8; 4096];
    volume[3..11].copy_from_slice(b"NTFS    ");
    volume[510] = 0x55;
    volume[511] = 0xAA;

    let mut file = std::fs::OpenOptions::new().write(true).open(path).expect("open foreign image");
    file.seek(SeekFrom::Start(at)).expect("seek");
    file.write_all(&volume).expect("write the foreign volume's first block");
    (at, bytes)
}

/// The `n` bytes at `at`, for a premise that is about one block of the image
/// rather than about all of it.
fn front(path: &Path, at: u64, n: usize) -> Vec<u8> {
    use std::io::{Read, Seek, SeekFrom};

    let mut head = vec![0u8; n];
    let mut file = std::fs::File::open(path).expect("open image");
    file.seek(SeekFrom::Start(at)).expect("seek into the image");
    file.read_exact(&mut head).expect("read the front of the volume");
    head
}

/// The shared-object cache's two refusals, judged in
/// `tests/toyos-rust-tests/src/bin/so_cache_policy.rs`. The independent oracle is
/// the NVMe image: the replaced library's bytes are read off the device after
/// the shutdown, so the claim rests on nothing the guest says.
pub fn so_cache_refusals(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    /// Without it the budget arm would have to load 256 MiB of libraries.
    const PARAMS: &[&str] = &["so-cache-tiny"];
    /// Mirrored in the guest binary; `/home` is a directory of DATA, so that is
    /// the name the host reader sees on the volume.
    const STALE: &str = "home/so-cache-stale.so";
    const SECOND: &str = "libtls_dlopen_lib.so";

    let want = rust_bins
        .iter()
        .find(|(name, _)| name == SECOND)
        .map(|(_, data)| data.clone())
        .ok_or_else(|| format!("{SECOND} was not built, so there is nothing to compare against"))?;

    let mut qemu = QemuInstance::boot_with_options(
        test_config,
        c_bins,
        rust_bins,
        BootOptions {
            profile: qemu::Profile::MetalDisk,
            kernel_params: PARAMS,
            ..Default::default()
        },
    );
    let boot = qemu.boot_log().to_string();
    if boot.contains("are a tmpfs") {
        return Err(format!(
            "/apps and /home fell back to tmpfs, so the readback below would judge no device:\n{boot}"
        ));
    }

    let result = qemu.run_test("test_rs_so_cache_policy", Duration::from_secs(60));
    let log = format!("{boot}\n{}{}{}", result.before, result.stdout, result.serial);
    if result.exit_code != Some(0) {
        return Err(format!(
            "so_cache_policy guest failed:\n{}\nkernel log while it ran:\n{}{}",
            result.stdout, result.before, result.serial
        ));
    }
    // Stated by the kernel too: an arm reporting a refusal nobody made would pass.
    for said in ["the cached image is stale", "byte budget; refused"] {
        if !log.contains(said) {
            return Err(format!("no {said:?} line — the kernel refused nothing:\n{log}"));
        }
    }

    let image = qemu.nvme_image().to_path_buf();
    writeln!(qemu.stdin_mut(), "run shutdown").expect("write to QEMU stdin");
    qemu.flush_stdin();
    let tail = qemu.drain_serial(Duration::from_secs(20));
    drop(qemu);
    for bad in ["PANIC:", "panicked at"] {
        if tail.contains(bad) {
            return Err(format!("{bad:?} on the way down\n{tail}"));
        }
    }

    let io = FileBlocks::open(&image)?;
    let fs = bcachefs::Mounted::<_, bcachefs::ReadOnly>::open(io)
        .map_err(|e| format!("the NVMe image does not mount on the host: {e:?}"))?;
    let got = fs
        .read_file(STALE)
        .map_err(|e| format!("reading {STALE} off the image: {e:?}"))?;
    if got != want {
        let at = got.iter().zip(&want).position(|(a, b)| a != b);
        return Err(format!(
            "{STALE} on the device is {} bytes against {SECOND}'s {}, first differing at {at:?} \
             — the guest's second write did not reach the device, so the refusal above was \
             about a file that had not changed",
            got.len(),
            want.len()
        ));
    }

    eprintln!(
        "  [so-cache] {} bytes of {SECOND} byte-identical at {STALE} off the NVMe image via the \
         host's own bcachefs reader",
        want.len()
    );
    Ok(())
}

/// F9's negative control: an fsync on `/home` whose first attempt is
/// budget-refused (`fsync-budget-spent`) must be retried to durable — on the
/// erasing adapter the `BudgetExpired` came back as `Io` and the guest's
/// `sync_all` failed on attempt 1. The independent oracle is the NVMe image
/// itself: after the shutdown the file's bytes are read off it on the host,
/// through this crate's own build of the `bcachefs` reader over a plain
/// seek-and-read device — nothing the guest kernel executed.
pub fn home_budget_refusal_retried(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    const PARAMS: &[&str] = &["fsync-budget-spent"];
    /// Mirrored in `tests/toyos-rust-tests/src/bin/home_fsync_budget.rs`, under
    /// the `/home` directory of DATA the host reader sees.
    const PATH: &str = "home/f9-budget.bin";
    const LEN: usize = 3 * 4096 + 41;
    fn pattern() -> Vec<u8> {
        (0..LEN).map(|i| (i.wrapping_mul(151) ^ 0x3C) as u8).collect()
    }

    let mut qemu = QemuInstance::boot_with_options(
        test_config,
        c_bins,
        rust_bins,
        BootOptions {
            profile: qemu::Profile::MetalDisk,
            kernel_params: PARAMS,
            ..Default::default()
        },
    );
    let boot = qemu.boot_log().to_string();
    if boot.contains("are a tmpfs") {
        return Err(format!(
            "/apps and /home fell back to tmpfs, so nothing below touches the NVMe path:\n{boot}"
        ));
    }

    let result = qemu.run_test("test_rs_home_fsync_budget", Duration::from_secs(30));
    let log = format!("{boot}\n{}{}{}", result.before, result.stdout, result.serial);
    if result.exit_code != Some(0) {
        return Err(format!(
            "home_fsync_budget guest failed — a budget-refused /home fsync was not retried \
             to durable:\n{}\nkernel log while it ran:\n{}{}",
            result.stdout, result.before, result.serial
        ));
    }
    // Both halves of the staging, or the arm proved nothing: the refusal at the
    // shipped NVMe site, and the fsync loop's own retry verdict.
    if !log.contains("not issued") {
        return Err(format!(
            "no `not issued` line, so `fsync-budget-spent` staged no NVMe refusal:\n{log}"
        ));
    }
    let retried = log
        .lines()
        .find(|l| l.contains("fsync: /home/") && l.contains("durable on attempt"))
        .ok_or_else(|| {
            format!("no `fsync: /home/... durable on attempt` line — the retry never ran:\n{log}")
        })?
        .trim()
        .to_string();

    let image = qemu.nvme_image().to_path_buf();
    writeln!(qemu.stdin_mut(), "run shutdown").expect("write to QEMU stdin");
    qemu.flush_stdin();
    let tail = qemu.drain_serial(Duration::from_secs(20));
    drop(qemu);
    for bad in ["PANIC:", "panicked at"] {
        if tail.contains(bad) {
            return Err(format!("{bad:?} on the way down\n{tail}"));
        }
    }

    let io = FileBlocks::open(&image)?;
    let fs = bcachefs::Mounted::<_, bcachefs::ReadOnly>::open(io)
        .map_err(|e| format!("the NVMe image does not mount on the host: {e:?}"))?;
    let got = fs
        .read_file(PATH)
        .map_err(|e| format!("reading {PATH} off the image: {e:?}"))?;
    if got != pattern() {
        let at = got.iter().zip(pattern()).position(|(a, b)| *a != b);
        return Err(format!(
            "{PATH} on the device is {} bytes, first differing at {at:?} — the retried fsync \
             reported durable over bytes the device does not hold",
            got.len()
        ));
    }

    eprintln!("  [f9] {retried}");
    eprintln!(
        "  [f9] {LEN} bytes byte-identical off the NVMe image via the host's own bcachefs reader"
    );
    Ok(())
}

/// A same-length overwrite on `/home` read back through the name it rebound.
///
/// The oracle is outside the guest and outside the kernel: once the machine is
/// gone the file is read off the NVMe image by this crate's own build of the
/// `bcachefs` reader over a plain seek-and-read device, and its length is
/// compared against the length the guest printed for its own read of the same
/// name. The recorded defect is exactly those two disagreeing — the guest read
/// 0 bytes while the device held the whole file.
pub fn home_overwrite_reads_back(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    /// Mirrored in `tests/toyos-rust-tests/src/bin/home_overwrite_zero.rs`,
    /// under the `/home` directory of DATA the host reader sees.
    const PINNED: &str = "home/overwrite-pinned.bin";
    const LEN: usize = 1_902_104;
    fn payload(seed: u8) -> Vec<u8> {
        (0..LEN).map(|i| (i.wrapping_mul(131) ^ seed as usize) as u8).collect()
    }

    let mut qemu = QemuInstance::boot_with_options(
        test_config,
        c_bins,
        rust_bins,
        BootOptions { profile: qemu::Profile::MetalDisk, ..Default::default() },
    );
    let boot = qemu.boot_log().to_string();
    if boot.contains("are a tmpfs") {
        return Err(format!(
            "/apps and /home fell back to tmpfs, so nothing below touches the NVMe path:\n{boot}"
        ));
    }

    let result = qemu.run_test("test_rs_home_overwrite_zero", Duration::from_secs(240));
    let log = format!("{boot}\n{}{}{}", result.before, result.stdout, result.serial);
    let said = log.lines().find(|l| l.contains("HOME-OVERWRITE")).map(str::trim).map(String::from);
    let guest_len: Option<usize> = said
        .as_deref()
        .and_then(|l| l.split_whitespace().rev().nth(1))
        .and_then(|n| n.parse().ok());

    let image = qemu.nvme_image().to_path_buf();
    writeln!(qemu.stdin_mut(), "run shutdown").expect("write to QEMU stdin");
    qemu.flush_stdin();
    let tail = qemu.drain_serial(Duration::from_secs(20));
    drop(qemu);
    for bad in ["PANIC:", "panicked at"] {
        if tail.contains(bad) {
            return Err(format!("{bad:?} on the way down\n{tail}"));
        }
    }

    let io = FileBlocks::open(&image)?;
    let fs = bcachefs::Mounted::<_, bcachefs::ReadOnly>::open(io)
        .map_err(|e| format!("the NVMe image does not mount on the host: {e:?}"))?;
    let got = fs
        .read_file(PINNED)
        .map_err(|e| format!("reading {PINNED} off the image: {e:?}"))?;

    // The device against the guest's own read, before the exit code: that
    // disagreement is the defect's sentence, and an exit code does not say it.
    let Some(guest_len) = guest_len else {
        return Err(format!(
            "the guest printed no HOME-OVERWRITE line, and the device holds {} bytes at \
             {PINNED}:\n{}{}{}",
            got.len(),
            result.before,
            result.stdout,
            result.serial
        ));
    };
    if guest_len != got.len() {
        return Err(format!(
            "the guest read {guest_len} bytes back from /{PINNED} and the device holds {} — \
             the overwrite reached the device and the name did not answer for it\n{}",
            got.len(),
            said.unwrap_or_default()
        ));
    }
    if got != payload(0x22) {
        let at = got.iter().zip(payload(0x22)).position(|(a, b)| *a != b);
        return Err(format!(
            "{PINNED} on the device is {} bytes, first differing at {at:?}",
            got.len()
        ));
    }
    if result.exit_code != Some(0) {
        return Err(format!(
            "home_overwrite_zero guest failed:\n{}\nkernel log while it ran:\n{}{}",
            result.stdout, result.before, result.serial
        ));
    }

    eprintln!(
        "  [overwrite] the guest's {guest_len} bytes and the device's {} agree at /{PINNED}, off \
         the NVMe image via the host's own bcachefs reader",
        got.len()
    );
    Ok(())
}

/// `/apps` and `/home` are two paths into one filesystem, judged off the device.
///
/// The guest writes one file under each and shuts down; the host then finds
/// both in **one** bcachefs volume on the NVMe image, through this crate's own
/// build of the reader over a plain seek-and-read device. A second filesystem
/// behind the second path could not answer for both names out of one mount.
pub fn apps_and_home_are_one_filesystem(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    /// Mirrored in `tests/toyos-rust-tests/src/bin/hierarchy_paths.rs`, without
    /// the mount point: the volume carries `/home/x` as `home/x`.
    const IN_HOME: &str = "home/hierarchy-home.bin";
    const IN_APPS: &str = "apps/hierarchy-apps.bin";
    const LEN: usize = 2 * 4096 + 61;
    fn payload(seed: u8) -> Vec<u8> {
        (0..LEN).map(|i| (i.wrapping_mul(53) ^ seed as usize) as u8).collect()
    }

    let mut qemu = QemuInstance::boot_with_options(
        test_config,
        c_bins,
        rust_bins,
        BootOptions { profile: qemu::Profile::MetalDisk, ..Default::default() },
    );
    let boot = qemu.boot_log().to_string();
    if boot.contains("are a tmpfs") {
        return Err(format!(
            "/apps and /home fell back to tmpfs, so the readback below would judge no device:\n\
             {boot}"
        ));
    }

    let result = qemu.run_test("test_rs_hierarchy_paths", Duration::from_secs(60));
    if result.exit_code != Some(0) {
        return Err(format!(
            "hierarchy_paths guest failed:\n{}\nkernel log while it ran:\n{}{}",
            result.stdout, result.before, result.serial
        ));
    }

    let image = qemu.nvme_image().to_path_buf();
    writeln!(qemu.stdin_mut(), "run shutdown").expect("write to QEMU stdin");
    qemu.flush_stdin();
    let tail = qemu.drain_serial(Duration::from_secs(20));
    drop(qemu);
    for bad in ["PANIC:", "panicked at"] {
        if tail.contains(bad) {
            return Err(format!("{bad:?} on the way down\n{tail}"));
        }
    }

    // Which volume this is, taken from the volume and not from the reader:
    // `Formatted::format` leaves the UUID zero on one nothing named, so a UUID
    // here would be a constant every image satisfies. The block count the
    // superblock records is not — it says the guest formatted this partition
    // and no other span of the device.
    let (at, bytes) = toyos_build::image::data_partition_of(&image)?;
    let blocks = bytes / 4096;
    let sb = superblock_at(&image, at / 4096)?;
    if sb.block_count != blocks {
        return Err(format!(
            "the volume on the image was formatted for {} blocks and the DATA partition is \
             {blocks}",
            sb.block_count
        ));
    }

    let io = FileBlocks::open(&image)?;
    let fs = bcachefs::Mounted::<_, bcachefs::ReadOnly>::open(io)
        .map_err(|e| format!("the NVMe image's DATA partition does not mount: {e:?}"))?;
    for (name, seed) in [(IN_HOME, 0xA5u8), (IN_APPS, 0x5A)] {
        let got = fs
            .read_file(name)
            .map_err(|e| format!("reading {name} off the DATA partition: {e:?}"))?;
        if got != payload(seed) {
            let at = got.iter().zip(payload(seed)).position(|(a, b)| *a != b);
            return Err(format!(
                "{name} on the device is {} bytes, first differing at {at:?}",
                got.len()
            ));
        }
    }

    eprintln!(
        "  [hierarchy] {IN_HOME} and {IN_APPS}, {LEN} bytes each, both in the one filesystem the \
         DATA partition at byte {at} carries, formatted for its own {blocks} blocks"
    );
    Ok(())
}

/// `/boot` and `/log` off the same NVMe device the page cache serves.
///
/// The oracle is outside the guest and outside the kernel's FAT32: `logd`'s
/// file is read off the image by `fatfs` and the volume judged against
/// fatgen103 by `toyos-fat32-check`, with the guest already halted.
pub fn internal_disk_boot(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let options = || BootOptions {
        profile: qemu::Profile::InternalDisk,
        boot_image: None,
        ..Default::default()
    };

    // The argv is the only place a device's *absence* is visible.
    let argv = qemu::profile_argv(&options());
    for banned in ["usb-storage", "nec-usb-xhci", "usb-kbd", "usb-mouse", "usb-tablet"] {
        if let Some(a) = argv.iter().find(|a| a.contains(banned)) {
            return Err(format!("{a:?} on the machine whose point is having no USB disk"));
        }
    }
    let controllers: Vec<&String> =
        argv.iter().filter(|a| a.starts_with("nvme,serial=")).collect();
    if controllers != ["nvme,serial=bootdisk,id=nvmebootctl,bootindex=0"] {
        return Err(format!(
            "the machine's NVMe controllers are {controllers:?} — this profile's whole shape is \
             one controller, carrying the boot image"
        ));
    }

    // Built here, not by `boot_with_options`: the log partition is read back off this exact file.
    let dir = super::lane::dir();
    let image = dir.join("internal-disk-boot.img");
    let bytes = qemu::build_boot_image(test_config, c_bins, rust_bins, &[]);
    std::fs::write(&image, &bytes).map_err(|e| format!("write the boot image: {e}"))?;
    let (log_start, log_len) = super::volumes::log_extent(&bytes, &image)?;

    let mut qemu = QemuInstance::boot_with_options(
        test_config,
        c_bins,
        rust_bins,
        BootOptions { boot_image: Some(image.clone()), ..options() },
    );
    let boot = qemu.boot_log().to_string();
    for bad in ["PANIC:", "panicked at"] {
        if boot.contains(bad) {
            return Err(format!("{bad:?} booting off the internal disk\n{boot}"));
        }
    }

    // `1` is NVMe's fixed `DeviceId` and the USB range starts at 16, so naming
    // it is also the assertion that no stick served either mount.
    for said in [
        "gpt: device 1 carries the boot partition",
        "gpt: device 1 carries the log partition",
        "boot-volume: partition mounted",
        "log-volume: partition mounted",
    ] {
        if !boot.contains(said) {
            return Err(format!(
                "the kernel never said {said:?} — a machine booting off its internal disk got \
                 no /boot and no /log\n{boot}"
            ));
        }
    }
    if boot.contains("no driver here can open it") {
        return Err(format!(
            "the kernel found the partition and had no second handle to the device carrying \
             it\n{boot}"
        ));
    }

    // Down, not killed: the file logd wrote reaches the device on the way out.
    writeln!(qemu.stdin_mut(), "run shutdown").expect("write to QEMU stdin");
    qemu.flush_stdin();
    let tail = qemu.drain_serial(Duration::from_secs(20));
    for bad in ["PANIC:", "panicked at"] {
        if tail.contains(bad) {
            return Err(format!("{bad:?} on the way down\n{tail}"));
        }
    }
    drop(qemu);

    let after = std::fs::read(&image).map_err(|e| format!("read the boot image back: {e}"))?;
    let volume = &after[log_start..log_start + log_len];
    let complaints = toyos_fat32_check::check(volume);
    if !complaints.is_empty() {
        return Err(format!(
            "the log volume the internal-disk boot left behind is not a FAT32 fatgen103 \
             recognises:\n{}",
            toyos_fat32_check::describe(&complaints)
        ));
    }
    let (name, on_device) = super::volumes::newest_log(&image, log_start, log_len)?;
    if on_device.is_empty() {
        return Err(format!("/log/{name} on the internal disk is empty"));
    }
    let text = String::from_utf8_lossy(&on_device);
    if !text.contains("Boot: complete") {
        return Err(format!(
            "/log/{name} is {} bytes off the device and carries no boot record — logd mounted \
             nothing worth writing to\nit ends: {:?}",
            on_device.len(),
            text.lines().rev().take(3).collect::<Vec<_>>().join(" | ")
        ));
    }

    let _ = std::fs::remove_file(&image);
    eprintln!(
        "  [internal-disk] /boot and /log both off NVMe device 1, and /log/{name} came back \
         {} bytes through fatfs on a volume fatgen103 has nothing to say about",
        on_device.len()
    );
    Ok(())
}

/// A write through a page cache whose view does not begin at block 0 lands at
/// the device's block, not the view's.
///
/// The oracle is the NVMe image after the guest has gone: the mark at
/// `(FIRST + AT) * 4096` and absent at `AT * 4096` is the partition offset in
/// `BlockKey` and nothing else. A foreign disk, so the kernel refuses to format
/// it and the probe's one block is the only byte this boot writes to it.
pub fn page_cache_partition_offset(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    /// Mirrored in `kernel/src/page_cache.rs::offset_probe`.
    const FIRST: u64 = 3000;
    const AT: u64 = 7;
    const MARK: &[u8] = b"TOYOS-PARTITION-OFFSET";
    const BYTES: u64 = 128 * 1024 * 1024;

    let dir = super::lane::dir();
    let image = dir.join("partition-offset.img");
    foreign_disk_image(&image, BYTES);

    let mut qemu = QemuInstance::boot_with_options(
        test_config,
        c_bins,
        rust_bins,
        BootOptions {
            profile: qemu::Profile::Metal,
            kernel_params: &["pc-partition-offset"],
            nvme_image: Some(image.clone()),
            ..Default::default()
        },
    );
    let boot = qemu.boot_log().to_string();
    for bad in ["PANIC:", "panicked at"] {
        if boot.contains(bad) {
            return Err(format!("{bad:?} with the offset probe armed\n{boot}"));
        }
    }
    let verdict = boot
        .lines()
        .find(|l| l.contains("pc-partition-offset: "))
        .ok_or_else(|| format!("the kernel never ran the offset probe:\n{boot}"))?
        .trim()
        .to_string();
    for want in [
        format!("landed_at_{}=true", FIRST + AT),
        format!("at_block_{AT}=false"),
    ] {
        if !verdict.contains(&want) {
            return Err(format!(
                "a cache over a view at +{FIRST} did not write where the view says — {want:?} is \
                 missing from: {verdict}"
            ));
        }
    }

    writeln!(qemu.stdin_mut(), "run shutdown").expect("write to QEMU stdin");
    qemu.flush_stdin();
    let tail = qemu.drain_serial(Duration::from_secs(20));
    for bad in ["PANIC:", "panicked at"] {
        if tail.contains(bad) {
            return Err(format!("{bad:?} on the way down\n{tail}"));
        }
    }
    drop(qemu);

    let after = std::fs::read(&image).map_err(|e| format!("read the image back: {e}"))?;
    let at = |block: u64| {
        let start = (block * 4096) as usize;
        after[start..start + MARK.len()].to_vec()
    };
    if at(FIRST + AT) != MARK {
        return Err(format!(
            "device block {} holds {:?} on the image, not the mark — the offset never reached \
             the write",
            FIRST + AT,
            String::from_utf8_lossy(&at(FIRST + AT))
        ));
    }
    if at(AT) == MARK {
        return Err(format!(
            "the mark is at device block {AT} on the image — the view's own block number went to \
             the device unchanged"
        ));
    }

    let _ = std::fs::remove_file(&image);
    eprintln!("  [offset] {verdict}, and the image agrees off the device");
    Ok(())
}

/// The impostor the actuator offers fills every read with its own mark, so a
/// registry that took it is caught serving that mark for a device it is not.
pub fn block_duplicate_id(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    const PARAMS: &[&str] = &["block-duplicate-id"];

    let qemu = QemuInstance::boot_with_options(
        test_config,
        c_bins,
        rust_bins,
        BootOptions {
            profile: qemu::Profile::Metal,
            kernel_params: PARAMS,
            ..Default::default()
        },
    );
    let boot = qemu.boot_log().to_string();
    for bad in ["PANIC:", "panicked at"] {
        if boot.contains(bad) {
            return Err(format!("{bad:?}: refusing a duplicate id must not be fatal\n{boot}"));
        }
    }

    let verdict = boot
        .lines()
        .find(|l| l.contains("block-duplicate-id: "))
        .ok_or_else(|| format!("the kernel never staged the duplicate registration:\n{boot}"))?
        .trim()
        .to_string();

    // `by_impostor` catches a table whose insert displaces; the count below
    // catches one that appends. Both naive registries, both silent.
    for want in [
        "refused=true",
        "block 0 served=true",
        "by_impostor=false",
    ] {
        if !verdict.contains(want) {
            return Err(format!(
                "a second device claiming a registered number was not refused — {want:?} is \
                 missing from: {verdict}"
            ));
        }
    }
    let counts: Vec<&str> = verdict
        .split("devices ")
        .nth(1)
        .unwrap_or_default()
        .split(", block 0")
        .next()
        .unwrap_or_default()
        .split(" before and ")
        .collect();
    match counts.as_slice() {
        [before, after] if after.trim_end_matches(" after") == *before => {}
        _ => {
            return Err(format!(
                "the device table changed size across a refused registration: {verdict}"
            ))
        }
    }
    if !boot.contains("Boot: complete") {
        return Err(format!("the boot did not complete\n{boot}"));
    }

    eprintln!("  [block] {verdict}");
    Ok(())
}

/// The bcachefs superblock in `image` at device block `block`.
pub fn superblock_at(image: &Path, block: u64) -> Result<bcachefs::Superblock, String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(image).map_err(|e| format!("open {}: {e}", image.display()))?;
    f.seek(SeekFrom::Start(block * 4096)).map_err(|e| format!("seek: {e}"))?;
    let mut buf = bcachefs::BlockBuf::zeroed();
    f.read_exact(buf.as_bytes_mut()).map_err(|e| format!("read: {e}"))?;
    bcachefs::Superblock::parse(&buf).map_err(|e| format!("{e:?}"))
}

/// A disk image's DATA partition as a bcachefs block device: plain
/// seek-and-read, no cache and no kernel code. The partition is located through
/// the table by `toyos-gpt`, never at an offset this side computed.
pub struct FileBlocks {
    file: std::cell::RefCell<std::fs::File>,
    first: u64,
    blocks: u64,
}

impl FileBlocks {
    pub fn open(path: &Path) -> Result<Self, String> {
        let (at, bytes) = toyos_build::image::data_partition_of(path)?;
        let file = std::fs::File::open(path)
            .map_err(|e| format!("open {}: {e}", path.display()))?;
        Ok(Self {
            file: std::cell::RefCell::new(file),
            first: at / 4096,
            blocks: bytes / 4096,
        })
    }
}

/// A host file's I/O failure was attempted and failed; nothing here budgets.
struct HostIoFailed;
impl bcachefs::TransferError for HostIoFailed {
    fn refused_before_attempt(&self) -> bool {
        false
    }
}

impl bcachefs::BlockIO for FileBlocks {
    fn read_block(
        &self,
        block: bcachefs::BlockNum,
        buf: &mut bcachefs::BlockBuf,
    ) -> Result<(), bcachefs::DeviceError> {
        use std::io::{Read, Seek, SeekFrom};
        let mut file = self.file.borrow_mut();
        file.seek(SeekFrom::Start((self.first + block.raw()) * 4096))
            .map_err(|_| bcachefs::DeviceError::classify(&HostIoFailed))?;
        file.read_exact(buf.as_bytes_mut()).map_err(|_| bcachefs::DeviceError::classify(&HostIoFailed))
    }

    fn write_block(
        &self,
        _block: bcachefs::BlockNum,
        _buf: &bcachefs::BlockBuf,
    ) -> Result<(), bcachefs::DeviceError> {
        Err(bcachefs::DeviceError::classify(&HostIoFailed))
    }

    fn block_count(&self) -> u64 {
        self.blocks
    }
}
