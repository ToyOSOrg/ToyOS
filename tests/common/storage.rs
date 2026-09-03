//! The interlock that keeps ToyOS off a disk it was not given.
//!
//! The claim under test is not "formatting works" -- `nvme_large_device` has
//! that -- but its negative: **a device the kernel was not given comes back
//! byte-for-byte unchanged.** That is asserted against the backing file, on
//! the host, because the guest's account of what it did to a disk is exactly
//! the thing in question.
//!
//! The stimulus is the state that caused this: a disk that holds something,
//! mounts as nothing, and belongs to someone. The kernel used to read "mount
//! returned None" as permission to format, so the first boot on the T14 would
//! have taken the owner's disk -- and the only reason the real first boot did
//! not is that an unrelated panic in `page_cache::init` came first, which has
//! since been fixed.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use toyos_build::fingerprint::{first_difference, whole_device};

use super::qemu::{self, BootOptions, QemuInstance};

/// Boot the guest against a disk that belongs to somebody else, and prove it
/// comes back untouched.
///
/// Lives here rather than in `toyos.rs` so that the registration hunk in that
/// shared file stays one line: every agent edits it, and a wide diff there is
/// how work gets swept into somebody else's commit.
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
    foreign_disk_image(&image, BYTES);
    let before = whole_device(&image);

    // The premise, checked before the boot rather than assumed: if this image
    // somehow already parsed as a ToyOS volume, the kernel would mount it and
    // the assertion below would pass for the wrong reason.
    if front(&image, 4) == *b"BCFS" {
        return Err("the foreign image starts with a bcachefs superblock".to_string());
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

    // Shut down rather than kill. `PageCache::sync` at shutdown is the only
    // thing that moves a format from the cache to the device, so killing QEMU
    // here would fingerprint an image that a formatting kernel had also left
    // untouched -- which is exactly what the first version of this test did,
    // and the negative gate caught it passing against a kernel that formatted.
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

    std::fs::write(&image, &volume).map_err(|e| format!("write the copied volume: {e}"))?;
    std::fs::OpenOptions::new()
        .write(true)
        .open(&image)
        .and_then(|f| f.set_len(DEVICE_BYTES))
        .map_err(|e| format!("grow the device under the volume: {e}"))?;
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

/// A GPT protective MBR and a plausible partition header: what the front of a
/// disk with another operating system on it looks like. Nothing in it is a
/// bcachefs superblock and nothing in it is a designation stamp, which is the
/// only property that matters -- but a recognisable layout is what makes a
/// failure legible, since the diff below prints where the bytes changed.
pub fn foreign_disk_image(path: &Path, len: u64) {
    use std::io::{Seek, SeekFrom, Write};

    let file = std::fs::File::create(path).expect("create foreign image");
    file.set_len(len).expect("size foreign image");

    let mut mbr = [0u8; 512];
    // One 0xEE partition spanning the disk, then the MBR signature.
    mbr[446] = 0x00;
    mbr[450] = 0xEE;
    mbr[454..458].copy_from_slice(&1u32.to_le_bytes());
    mbr[458..462].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    mbr[510] = 0x55;
    mbr[511] = 0xAA;

    let mut gpt = [0u8; 512];
    gpt[..8].copy_from_slice(b"EFI PART");
    gpt[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());

    let mut file = std::fs::OpenOptions::new().write(true).open(path).expect("open foreign image");
    file.seek(SeekFrom::Start(0)).expect("seek");
    file.write_all(&mbr).expect("write mbr");
    file.write_all(&gpt).expect("write gpt header");
}

/// The first `n` bytes of `path`, for a premise that is about the front of the
/// image rather than about all of it.
fn front(path: &Path, n: usize) -> Vec<u8> {
    use std::io::Read;

    let mut head = vec![0u8; n];
    std::fs::File::open(path)
        .expect("open image")
        .read_exact(&mut head)
        .expect("read the front of the image");
    head
}

/// The shared-object cache's two refusals, judged in
/// `tests/toyos-rust-tests/src/bin/so_cache_policy.rs`. The independent oracle
/// is the NVMe image: after the shutdown the replaced library's bytes are read
/// off the device through this crate's own build of the `bcachefs` reader over a
/// plain seek-and-read file, so the claim rests on no part of the guest's own
/// account of itself.
pub fn so_cache_refusals(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    /// Without it the budget arm would have to load 256 MiB of libraries.
    const PARAMS: &[&str] = &["so-cache-tiny"];
    /// Mirrored in the guest binary, on the mount root the host reader sees.
    const STALE: &str = "so-cache-stale.so";
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
    if boot.contains("/home is a tmpfs") {
        return Err(format!(
            "/home fell back to tmpfs, so the readback below would judge no device:\n{boot}"
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
    /// Mirrored in `tests/toyos-rust-tests/src/bin/home_fsync_budget.rs`.
    const PATH: &str = "f9-budget.bin";
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
    if boot.contains("/home is a tmpfs") {
        return Err(format!(
            "/home fell back to tmpfs, so nothing below touches the NVMe path:\n{boot}"
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

/// A disk image as a bcachefs block device: plain seek-and-read, no cache and
/// no kernel code.
struct FileBlocks {
    file: std::cell::RefCell<std::fs::File>,
    blocks: u64,
}

impl FileBlocks {
    fn open(path: &Path) -> Result<Self, String> {
        let file = std::fs::File::open(path)
            .map_err(|e| format!("open {}: {e}", path.display()))?;
        let len = file.metadata().map_err(|e| format!("stat: {e}"))?.len();
        Ok(Self { file: std::cell::RefCell::new(file), blocks: len / 4096 })
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
        file.seek(SeekFrom::Start(block.raw() * 4096))
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
