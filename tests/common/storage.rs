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

/// fsd's word for a DATA partition that holds neither a volume of ours nor a
/// designation stamp: nothing is written to it.
const FOREIGN: &str = "fsd: no volume of ours and no designation stamp";
/// fsd's word for a volume of ours that mounted.
const MOUNTED: &str = "fsd: mounted the DATA volume";
/// fsd's word for a volume of ours that did not, followed by the reason.
const UNMOUNTABLE: &str = "fsd: the DATA volume is ours and does not mount (";
/// fsd's word for DATA's directories served from memory.
pub(super) const IN_MEMORY: &str = "are in memory and will not survive a reboot";

/// Whether fsd said it serves DATA's directories as absent: every name under
/// them refused, and never a volume in memory under the paths an owner's data
/// lives at.
fn data_absent(log: &str) -> Result<(), String> {
    if log.lines().any(|l| l.contains("fsd: Data serving") && l.contains(" — absent: ")) {
        return Ok(());
    }
    Err(format!("fsd never said it serves DATA's directories absent\n{log}"))
}

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
    // The refusal is stated, not inferred. A file server that never reached
    // the partition would also leave the image untouched.
    if !log.contains(FOREIGN) {
        return Err(format!("fsd never said {FOREIGN:?} — did it reach the partition?\n{log}"));
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
        return Err(format!("fsd decided to format a disk it was not given\n{log}"));
    }

    // Shut down rather than kill: the shutdown's sync of every file server is
    // what moves a format from fsd's cache to the device, so a killed QEMU
    // fingerprints an image a formatting server would also have left untouched.
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
    if log.contains(MOUNTED) {
        return Err(format!(
            "fsd mounted a volume that did not come from this disk: it said {MOUNTED:?}\n{log}"
        ));
    }
    // A superblock of ours that does not describe this device is a volume of
    // ours that did not mount, not another's disk.
    let refused = format!("{UNMOUNTABLE}BadSuperblock");
    if !log.contains(&refused) {
        return Err(format!("fsd never said {refused:?} — did it reach the partition?\n{log}"));
    }
    if log.contains("formatting it") {
        return Err(format!("fsd decided to format a disk it was not given\n{log}"));
    }
    if !log.contains("Boot: complete") {
        return Err(format!("the boot did not complete on a volume it refused\n{log}"));
    }

    // Down through the shutdown's sync of every file server, the only thing
    // that moves a write out of fsd's cache and onto the device.
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

/// A DATA volume of ours whose superblock broke, both copies of it: the boot
/// goes on with `/apps` and `/home` absent and the reason logged by name, and
/// never on a tmpfs, which would take the owner's writes into RAM under the
/// paths their data lives at. Absent rather than a refused boot because a
/// corrupt disk is input, and input never takes the kernel down. The oracle for
/// "nothing wrote to it" is the image, compared byte for byte after shutdown.
pub fn broken_data_volume_is_absent(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    const DEVICE_BYTES: u64 = 128 * 1024 * 1024;
    /// Inside the bytes the superblock's CRC covers, and past every field.
    const FLIPPED: usize = 200;
    let dir = super::lane::dir();
    let image = dir.join("broken-data-volume.img");

    let file = std::fs::File::create(&image).map_err(|e| format!("create the image: {e}"))?;
    file.set_len(DEVICE_BYTES).map_err(|e| format!("size the image: {e}"))?;
    let (at, bytes) = toyos_build::image::designate_data_disk(&image, DEVICE_BYTES);
    let blocks = bytes / 4096;
    let mut fs = bcachefs::Formatted::format(bcachefs::VecBlockIO::new(blocks))
        .map_err(|e| format!("format the partition's volume on the host: {e:?}"))?;
    fs.create("home/kept.txt", b"the owner's file", 1)
        .map_err(|e| format!("put a file on the host volume: {e:?}"))?;
    let mut volume = fs.into_io().map_err(|e| format!("sync the host volume: {e:?}"))?.into_vec();

    // The premise, both halves: the volume mounts as written, so the refusal
    // below is the flipped bytes' and not the partition's size.
    let open = |raw: &[u8]| {
        bcachefs::Mounted::<_, bcachefs::ReadOnly>::open(bcachefs::VecBlockIO::from_vec(raw.to_vec()))
            .err()
            .map(|e| format!("{e:?}"))
    };
    if let Some(e) = open(&volume) {
        return Err(format!("the volume this test wrote does not mount before it is broken: {e}"));
    }
    let backup = (blocks as usize - 1) * 4096;
    volume[FLIPPED] ^= 0xFF;
    volume[backup + FLIPPED] ^= 0xFF;
    match open(&volume) {
        Some(e) if e.starts_with("ChecksumMismatch") => {}
        other => return Err(format!("both superblocks flipped, and the host says {other:?}")),
    }
    {
        use std::io::{Seek, SeekFrom};
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(&image)
            .map_err(|e| format!("open the image: {e}"))?;
        file.seek(SeekFrom::Start(at)).map_err(|e| format!("seek: {e}"))?;
        file.write_all(&volume).map_err(|e| format!("write the broken volume: {e}"))?;
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
            return Err(format!("{bad:?}: a broken volume must not be fatal\n{log}"));
        }
    }
    for said in [&format!("{UNMOUNTABLE}ChecksumMismatch"), "Boot: complete"] {
        if !log.contains(said) {
            return Err(format!("the boot never said {said:?}\n{log}"));
        }
    }
    data_absent(&log)?;
    for unsaid in [IN_MEMORY, MOUNTED, "formatting it", FOREIGN] {
        if log.contains(unsaid) {
            return Err(format!("fsd said {unsaid:?} of a volume of ours that broke\n{log}"));
        }
    }

    // The kernel's own log line and the unchanged image are not a guest's
    // account of what it sees: this asks one, in the same boot, before
    // anything is asleep to answer for /home the way a stray tmpfs mount or a
    // `home` still forced true would.
    let result = qemu.run_test("test_rs_home_absent", Duration::from_secs(20));
    if result.exit_code != Some(0) {
        return Err(format!(
            "home_absent guest failed — /apps, /config, /home, /state or /home/toy answered a write, \
             a chdir or a listing that an absent DATA volume must refuse:\n{}\nkernel log while it ran:\n{}{}",
            result.stdout, result.before, result.serial
        ));
    }

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
        return Err(format!("the kernel wrote to a volume it could not mount: {diff}"));
    }
    let _ = std::fs::remove_file(&image);
    eprintln!("  [storage] a broken DATA volume left /apps and /home absent, and the image unchanged");
    Ok(())
}

/// A TOYOS-DATA partition the GPT type names ours, but whose start blockd
/// refuses to serve: the owner's ruling is that this is
/// `Absent`, the same as a volume of ours that did not mount, and never
/// `Volatile` — a tmpfs is for a machine that carries no data volume at all,
/// not for one whose candidate is ours and unreadable by geometry.
pub fn data_candidate_with_bad_geometry_is_absent(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    const DEVICE_BYTES: u64 = 128 * 1024 * 1024;
    let dir = super::lane::dir();
    let image = dir.join("misaligned-data.img");

    let file = std::fs::File::create(&image).map_err(|e| format!("create the image: {e}"))?;
    file.set_len(DEVICE_BYTES).map_err(|e| format!("size the image: {e}"))?;
    toyos_build::image::misaligned_data_disk(&image, DEVICE_BYTES);
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
            return Err(format!("{bad:?}: a misaligned candidate must not be fatal\n{log}"));
        }
    }
    for said in ["is not served: LBA", "not whole", "Boot: complete"] {
        if !log.contains(said) {
            return Err(format!("the boot never said {said:?}\n{log}"));
        }
    }
    data_absent(&log)?;
    for unsaid in [IN_MEMORY, MOUNTED, "formatting it"] {
        if log.contains(unsaid) {
            return Err(format!("fsd said {unsaid:?} of a partition its own GPT type names ours\n{log}"));
        }
    }

    let result = qemu.run_test("test_rs_home_absent", Duration::from_secs(20));
    if result.exit_code != Some(0) {
        return Err(format!(
            "home_absent guest failed on a partition blockd refused:\n{}\nkernel log while it \
             ran:\n{}{}",
            result.stdout, result.before, result.serial
        ));
    }

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
        return Err(format!("the kernel wrote to a candidate it could not open a view over: {diff}"));
    }
    let _ = std::fs::remove_file(&image);
    eprintln!(
        "  [storage] a TOYOS-DATA candidate with bad geometry left /apps and /home absent, and \
         the image unchanged"
    );
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
/// `tests/toyos-rust-tests/src/bin/so_cache_policy.rs`. The cache answers for a
/// path the kernel opens itself, which is `/system` and `/tmp`, so the files
/// are tmpfs's: the guest holds the rewritten bytes against the library it
/// copied, and the kernel's own lines say it refused.
pub fn so_cache_refusals(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    /// Without it the budget arm would have to load 256 MiB of libraries.
    const PARAMS: &[&str] = &["so-cache-tiny"];

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

    writeln!(qemu.stdin_mut(), "run shutdown").expect("write to QEMU stdin");
    qemu.flush_stdin();
    let tail = qemu.drain_serial(Duration::from_secs(20));
    drop(qemu);
    for bad in ["PANIC:", "panicked at"] {
        if tail.contains(bad) {
            return Err(format!("{bad:?} on the way down\n{tail}"));
        }
    }
    eprintln!("  [so-cache] a changed file and a full budget refused, by the kernel's own word");
    Ok(())
}

/// A same-length overwrite on `/home` read back through the name it rebound.
///
/// The oracle is outside the guest and outside the kernel: with the machine
/// gone the file is read off the NVMe image by this crate's own build of the
/// `bcachefs` reader over a plain seek-and-read device, and its length held
/// against the length the guest printed for its own read of the same name. The
/// recorded defect is exactly those two disagreeing.
pub fn home_overwrite_reads_back(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    /// Mirrored in `tests/toyos-rust-tests/src/bin/home_overwrite_zero.rs`.
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
    if boot.contains(IN_MEMORY) {
        return Err(format!(
            "/apps and /home fell back to memory, so nothing below touches the NVMe path:\n{boot}"
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

    // Before the exit code: that disagreement is the defect's sentence, and an exit code does not say it.
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

/// A file server killed with a write done and unanswered loses nothing a
/// client was told was flushed, and its clients go on; ended past init's
/// budget, its directories answer `Gone`. Judged off the device.
///
/// `tests/fsdrestartcase` arms every file server to end under a write to
/// `/home/fsd_end` (`--end-on`) and at the first read of an installed
/// package's manifest and binary (`--end-at-read`), and `test_rs_fs_restart`
/// ends DATA's four times, the first two under init's own resolution of a
/// launch and its read of the image: the guest asserts what a client sees,
/// init's and fsd's own lines say who ended and who started again, and with
/// the machine down the DATA partition is read by this crate's own build of
/// the `bcachefs` reader over a plain seek-and-read of the image — nothing the
/// guest executed. The flushed file holds its bytes there.
///
/// `test_rs_fs_client_bound` runs first on the same boot, which nothing else
/// needs DATA on while it holds every client slot.
pub fn fsd_restart(
    _test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    /// Mirrored in `tests/toyos-rust-tests/src/bin/fs_restart.rs`, without the
    /// mount point.
    const KEPT: &str = "home/fs_restart/kept";
    const ACROSS: &str = "home/fs_restart/across";
    const KEPT_LEN: usize = 64 * 1024 + 13;
    const ACROSS_BYTES: &[u8] = b"renamed over the file a handle held across the end";
    let kept: Vec<u8> = (0..KEPT_LEN).map(|i| (i.wrapping_mul(37) ^ 0xC3) as u8).collect();

    let config = super::compile::repo_root().join("tests/fsdrestartcase");
    let mut qemu = QemuInstance::boot_with_options(
        &config,
        c_bins,
        rust_bins,
        BootOptions { profile: qemu::Profile::Metal, ..Default::default() },
    );
    let boot = qemu.boot_log().to_string();
    if !boot.contains(MOUNTED) && !boot.contains("formatting it") {
        return Err(format!("fsd served DATA from no partition, so nothing here reaches a device:\n{boot}"));
    }
    let bound = qemu.run_test("test_rs_fs_client_bound", Duration::from_secs(60));
    if bound.exit_code != Some(0) || !bound.stdout.contains("fs_client_bound: PASS") {
        return Err(format!("fs_client_bound guest failed:\n{}\nconsole:\n{}{}", bound.stdout, bound.before, bound.serial));
    }
    let result = qemu.run_test("test_rs_fs_restart", Duration::from_secs(120));
    let image = qemu.nvme_image().to_path_buf();
    writeln!(qemu.stdin_mut(), "run shutdown").expect("write to QEMU stdin");
    qemu.flush_stdin();
    let tail = qemu.drain_serial(Duration::from_secs(20));
    drop(qemu);
    // The console once: `stdout` is the same lines again, unprefixed.
    let log = format!("{boot}\n{}{}{}{}{tail}", bound.before, bound.serial, result.before, result.serial);
    if result.exit_code != Some(0) || !result.stdout.contains("fs_restart: PASS") {
        return Err(format!("fs_restart guest failed:\n{}\nconsole:\n{log}", result.stdout));
    }
    let console = super::serial::Serial::named("fsd_restart", log.as_str());
    let ended = log.matches("fsd: --end-on: ending with a write done and unanswered").count();
    if ended != 2 {
        return Err(format!("fsd said it ended under a write {ended} times, not the guest's 2:\n{log}"));
    }
    for read in ["apps/fs_restart/manifest.toml", "apps/fs_restart/fs_restart"] {
        let said = format!("fsd: --end-at-read: ending before the first read of {read} is answered");
        let at_read = log.matches(&said).count();
        if at_read != 1 {
            return Err(format!("fsd said {said:?} {at_read} times, not the launch's 1:\n{log}"));
        }
    }
    let restarted = log.lines().filter(|l| l.contains("init: fsd data (pid ") && l.contains("ended; started again")).count();
    if restarted != 3 {
        return Err(format!("init started DATA's server again {restarted} times, not 3:\n{log}"));
    }
    console.must_say("init: fsd data ended 4 times in 10 s; its ports are closed")?;
    console.must_be_clean()?;

    let io = FileBlocks::open(&image)?;
    let fs = bcachefs::Mounted::<_, bcachefs::ReadOnly>::open(io)
        .map_err(|e| format!("the DATA partition does not mount on the host: {e:?}"))?;
    for (name, want) in [(KEPT, &kept[..]), (ACROSS, ACROSS_BYTES)] {
        let got = fs.read_file(name).map_err(|e| format!("reading {name} off the image: {e:?}"))?;
        if got != want {
            let at = got.iter().zip(want).position(|(a, b)| a != b);
            return Err(format!(
                "{name} on the device is {} bytes, first differing at {at:?}: a write the guest \
                 was told was flushed is not what the device holds",
                got.len()
            ));
        }
    }
    eprintln!(
        "  [fsd] DATA's server ended four times, the first two under init's resolution and image \
         read of a launch that was answered; started again three, every handle held across an \
         end answered Gone, new opens answered, the fourth closed /home to Gone; {KEPT} and \
         {ACROSS} read back off the image by the host's own bcachefs reader"
    );
    Ok(())
}

/// DATA's first file server ending before it accepts a connection that init's
/// own file worker is waiting on costs init nothing: it starts the server
/// again, the waiting call goes on to it, and the boot reaches the ready
/// marker with the session home made. `tests/fsdmountcase` arms the end
/// (`--end-at-mount data`); the home is judged off the image by the host's own
/// bcachefs reader once the machine is down.
pub fn fsd_end_at_mount(
    _test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    const ENDED: &str = "fsd: --end-at-mount: ending with a connection waiting and unaccepted";
    let config = super::compile::repo_root().join("tests/fsdmountcase");
    let mut qemu = QemuInstance::boot_with_options(
        &config,
        c_bins,
        rust_bins,
        BootOptions { profile: qemu::Profile::Metal, ..Default::default() },
    );
    let boot = qemu.boot_log().to_string();
    let image = qemu.nvme_image().to_path_buf();
    writeln!(qemu.stdin_mut(), "run shutdown").expect("write to QEMU stdin");
    qemu.flush_stdin();
    let tail = qemu.drain_serial(Duration::from_secs(20));
    drop(qemu);
    let log = format!("{boot}\n{tail}");
    let ended = log.matches(ENDED).count();
    if ended != 1 {
        return Err(format!("fsd said {ENDED:?} {ended} times, not once:\n{log}"));
    }
    let restarted = log.lines().filter(|l| l.contains("init: fsd data (pid ") && l.contains("ended; started again")).count();
    if restarted != 1 {
        return Err(format!("init started DATA's server again {restarted} times, not once:\n{log}"));
    }
    let console = super::serial::Serial::named("fsd_end_at_mount", log.as_str());
    console.must_not_say("session home")?;
    console.must_be_clean()?;

    let home = toyos_manifest::session_home();
    let home = home.trim_start_matches('/');
    let fs = bcachefs::Mounted::<_, bcachefs::ReadOnly>::open(FileBlocks::open(&image)?)
        .map_err(|e| format!("the DATA partition does not mount on the host: {e:?}"))?;
    if !fs.is_dir(home).map_err(|e| format!("asking the image for {home}: {e:?}"))? {
        return Err(format!("{home} is not on the DATA volume: init made no session home\n{log}"));
    }
    eprintln!("  [fsd] DATA's first server ended under init's waiting call; init started it again and {home} is on the image");
    Ok(())
}

/// A partition claim held elsewhere refuses its file server's restart by name,
/// and the role's paths are then Gone: no server answers them.
///
/// DATA is on a USB stick the kernel drives, so its server holds the
/// partition's claim, and the NVMe disk carries no table, so the stick's is
/// the machine's one DATA. `tests/fsdclaimcase` arms `--let-go-at-read`, and
/// `test_rs_fs_claim_held` takes the claim the server let go, ends the server,
/// and holds the claim until init has answered the role's restart.
pub fn fsd_claim_held(
    _test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    /// Mirrored in `tests/toyos-rust-tests/src/bin/fs_claim_held.rs`.
    const DATA: &str = "7E2B4C6D-8F1A-4B3C-9D5E-6F7A8B9C0D1E";
    const MIB: u64 = 1024 * 1024;
    const LET_GO: &str = "fsd: --let-go-at-read: home/fsd_let_go: the partition is let go";
    const ENDED: &str = "fsd: --let-go-at-read: ending at its client's next request";
    const REFUSED: &str = "init: fsd data ended and would not start again (";
    const HELD: &str = "is already claimed); its ports are closed";

    let stick = super::lane::dir().join("fsd-claim-held.img");
    let data = ("ToyOS data", 96 * MIB, toyos_gpt::Guid::TOYOS_DATA_TEXT, DATA, super::partclaim::ALIGNED);
    let (mut device, spans) = super::partclaim::table(&stick, 100 * MIB, &[data])?;
    super::partclaim::designate(&mut *device, spans[0])?;
    device.flush().map_err(|e| format!("flush the stick: {e}"))?;
    drop(device);

    let config = super::compile::repo_root().join("tests/fsdclaimcase");
    let mut qemu = QemuInstance::boot_with_options(
        &config,
        c_bins,
        rust_bins,
        BootOptions {
            profile: qemu::Profile::UsbDisk,
            usb_images: vec![stick.clone()],
            nvme_image: Some(super::partclaim::tableless_nvme("fsd-claim-held-nvme.img")?),
            ..Default::default()
        },
    );
    let boot = qemu.boot_log().to_string();
    // The premise: DATA's first server holds the stick's partition.
    if !boot.contains("fsd: block 0 designates this partition for ToyOS; formatting it") {
        return Err(format!("fsd never formatted DATA off the stick, so no server held its claim:\n{boot}"));
    }
    let result = qemu.run_test("test_rs_fs_claim_held", Duration::from_secs(60));
    writeln!(qemu.stdin_mut(), "run shutdown").expect("write to QEMU stdin");
    qemu.flush_stdin();
    let tail = qemu.drain_serial(Duration::from_secs(20));
    drop(qemu);
    let log = format!("{boot}\n{}{}{tail}", result.before, result.serial);
    if result.exit_code != Some(0) || !result.stdout.contains("fs_claim_held: PASS") {
        return Err(format!("fs_claim_held guest failed:\n{}\nconsole:\n{log}", result.stdout));
    }
    let console = super::serial::Serial::named("fsd_claim_held", log.as_str());
    console.must_say(LET_GO)?;
    console.must_say(ENDED)?;
    let Some(refused) = log.lines().find(|l| l.contains(REFUSED) && l.contains(HELD)) else {
        return Err(format!("init never said DATA's restart was refused for the held claim:\n{log}"));
    };
    console.must_be_clean()?;
    let _ = std::fs::remove_file(&stick);
    eprintln!("  [fsd] {}", refused.trim());
    Ok(())
}

/// Two DATA partitions, one on a stick the kernel drives and one on the NVMe
/// disk blockd serves, are refused by name and never guessed between: DATA is
/// absent, nothing stands in from memory, and neither is formatted — the
/// stick is held byte for byte against what it carried before the boot.
pub fn fsd_two_data(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    const DATA: &str = "5A1C0E2B-3D4F-4A6B-8C9D-0E1F2A3B4C5D";
    const MIB: u64 = 1024 * 1024;
    const REFUSED: &str = "fsd: this machine has 2 DATA partitions, 1 on the kernel's disks and 1 the block \
                           service serves, and a volume is one; DATA is absent this boot";

    let stick = super::lane::dir().join("fsd-two-data.img");
    let data = ("ToyOS data", 96 * MIB, toyos_gpt::Guid::TOYOS_DATA_TEXT, DATA, super::partclaim::ALIGNED);
    let (mut device, spans) = super::partclaim::table(&stick, 100 * MIB, &[data])?;
    super::partclaim::designate(&mut *device, spans[0])?;
    device.flush().map_err(|e| format!("flush the stick: {e}"))?;
    drop(device);
    let before = whole_device(&stick);

    // The lane's blank NVMe image is the second: a designated DATA partition.
    let mut qemu = QemuInstance::boot_with_options(
        test_config,
        c_bins,
        rust_bins,
        BootOptions { profile: qemu::Profile::UsbDisk, usb_images: vec![stick.clone()], ..Default::default() },
    );
    let boot = qemu.boot_log().to_string();
    // Shut down rather than kill: a format sitting in a server's cache reaches
    // the device at the stop's sync, and the stick is judged after it.
    writeln!(qemu.stdin_mut(), "run shutdown").expect("write to QEMU stdin");
    qemu.flush_stdin();
    let tail = qemu.drain_serial(Duration::from_secs(20));
    drop(qemu);
    let log = format!("{boot}\n{tail}");
    let console = super::serial::Serial::named("fsd_two_data", log.as_str());
    console.must_say(REFUSED)?;
    data_absent(&log)?;
    console.must_not_say(IN_MEMORY)?;
    console.must_not_say("formatting it")?;
    console.must_be_clean()?;
    if let Some(diff) = first_difference(&before, &whole_device(&stick)) {
        return Err(format!("a DATA partition of two was written: {diff}\n{log}"));
    }
    let _ = std::fs::remove_file(&stick);
    eprintln!("  [fsd] two DATA partitions, one per source, refused by name; the stick untouched");
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
    if boot.contains(IN_MEMORY) {
        return Err(format!(
            "/apps and /home fell back to memory, so the readback below would judge no device:\n\
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

/// `/boot` and `/log` off the same NVMe device the machine booted from, both
/// served by fsd through blockd, which refuses ROOT to every session.
///
/// The oracle is outside the guest and outside fsd's FAT32: `logd`'s
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
    if controllers != ["nvme,serial=bootdisk,id=nvmebootctl,bootindex=0,msix-exclusive-bar=on"] {
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
        BootOptions { boot_image: Some(qemu::Staged::Written(image.clone())), ..options() },
    );
    let boot = qemu.boot_log().to_string();
    for bad in ["PANIC:", "panicked at"] {
        if boot.contains(bad) {
            return Err(format!("{bad:?} booting off the internal disk\n{boot}"));
        }
    }

    // This machine has no USB, so a volume fsd serves came through blockd.
    for said in [super::volumes::BOOT_SERVED, super::volumes::LOG_SERVED] {
        if !boot.contains(said) {
            return Err(format!(
                "the boot never said {said:?} — a machine booting off its internal disk got \
                 no /boot and no /log\n{boot}"
            ));
        }
    }
    if !boot.contains("this machine runs from; refusing every session to it") {
        return Err(format!("blockd never said it refuses ROOT, which the machine runs from\n{boot}"));
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
        "  [internal-disk] /boot and /log both off the boot NVMe through blockd, and /log/{name} came back \
         {} bytes through fatfs on a volume fatgen103 has nothing to say about",
        on_device.len()
    );
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

    /// The whole file as one volume, for an image that is not a partitioned
    /// disk: a raw device a guest formatted end to end.
    pub fn whole(path: &Path) -> Result<Self, String> {
        let file = std::fs::File::open(path)
            .map_err(|e| format!("open {}: {e}", path.display()))?;
        let bytes = file.metadata().map_err(|e| format!("stat {}: {e}", path.display()))?.len();
        Ok(Self { file: std::cell::RefCell::new(file), first: 0, blocks: bytes / 4096 })
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
