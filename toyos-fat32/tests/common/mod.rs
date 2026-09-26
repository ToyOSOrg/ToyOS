//! Host-side scaffolding: real FAT32 images made by macOS, devices that carry
//! them, and the volume-checker gate.
//!
//! Nothing here formats a volume. The images come from `newfs_msdos`, are
//! populated through a real mount, and are judged by `toyos-fat32-check`, which
//! is written from the specification and shares no code with this crate — so
//! the ground truth on both sides of every test is something other than the
//! driver under test.
//!
//! **A device here standing in for the kernel adapter models that adapter's
//! cache wherever the two can differ.** A write refused after it reached the
//! medium leaves `FatDevice`'s resident blocks and the medium disagreeing, so a
//! device that refuses writes that way serves reads through [`AdapterCache`]
//! and never straight from the medium, and a judge of the volume reads the
//! medium and never through the crate's device. A device that refuses only
//! before a write is issued keeps the two equal and needs no model.

#![allow(dead_code)]

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use toyos_fat32::{BlockAccess, IoError};

/// `hdiutil` hands out device nodes from one global pool and mounts into one
/// global `/Volumes`. Two tests attaching at once is a race in macOS, not in
/// this crate, so attach/detach pairs are serialised.
static HDIUTIL: Mutex<()> = Mutex::new(());

static LABEL_SEQ: AtomicU32 = AtomicU32::new(0);

fn scratch_root() -> PathBuf {
    let dir = std::env::temp_dir().join("toyos-fat32-tests");
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn run(cmd: &mut Command) -> (bool, String) {
    let out = cmd.output().unwrap_or_else(|e| panic!("failed to run {cmd:?}: {e}"));
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

/// Delete every `._*` AppleDouble sidecar `root` or a directory under it
/// holds — recursively, since macOS drops one beside any file it touches on
/// a mount at any depth. A directory that vanished mid-walk (macOS removing
/// its own sidecar concurrently) is not an error: there is nothing left in
/// it to delete.
fn delete_apple_double_sidecars(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            delete_apple_double_sidecars(&path);
        } else if path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("._")) {
            let _ = std::fs::remove_file(&path);
        }
    }
}

// ---------------------------------------------------------------- devices

/// A device backed by a file, which only ever issues whole 4096-byte block
/// reads and writes to it.
///
/// This is the kernel's `BlockDevice` shape, and it is what the tests run on
/// so that the claim in [`BlockAccess`]'s documentation — that a caller with a
/// 4096-byte block size can serve a driver working in 512-byte sectors — is
/// exercised rather than asserted. Every partial request becomes a
/// read-modify-write here, exactly as it will in the kernel adapter.
pub struct BlockyFile {
    file: File,
    capacity: u64,
    block: usize,
}

impl BlockyFile {
    pub fn open(path: &Path, block: usize) -> BlockyFile {
        let file = OpenOptions::new().read(true).write(true).open(path).expect("open image");
        let capacity = file.metadata().expect("stat image").len();
        BlockyFile { file, capacity, block }
    }

    fn check(&self, offset: u64, len: usize) -> Result<(), IoError> {
        let end = offset.checked_add(len as u64).ok_or(IoError::Device)?;
        if end > self.capacity {
            return Err(IoError::Device);
        }
        Ok(())
    }
}

impl BlockAccess for BlockyFile {
    fn capacity(&self) -> u64 {
        self.capacity
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), IoError> {
        self.check(offset, buf.len())?;
        let mut block = vec![0u8; self.block];
        let mut done = 0usize;
        while done < buf.len() {
            let pos = offset + done as u64;
            let base = pos / self.block as u64 * self.block as u64;
            let within = (pos - base) as usize;
            let n = (self.block - within).min(buf.len() - done);
            self.file.read_exact_at(&mut block, base).map_err(|_| IoError::Device)?;
            buf[done..done + n].copy_from_slice(&block[within..within + n]);
            done += n;
        }
        Ok(())
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> Result<(), IoError> {
        self.check(offset, buf.len())?;
        let mut block = vec![0u8; self.block];
        let mut done = 0usize;
        while done < buf.len() {
            let pos = offset + done as u64;
            let base = pos / self.block as u64 * self.block as u64;
            let within = (pos - base) as usize;
            let n = (self.block - within).min(buf.len() - done);
            self.file.read_exact_at(&mut block, base).map_err(|_| IoError::Device)?;
            block[within..within + n].copy_from_slice(&buf[done..done + n]);
            self.file.write_all_at(&block, base).map_err(|_| IoError::Device)?;
            done += n;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), IoError> {
        self.file.sync_all().map_err(|_| IoError::Device)
    }
}

/// A volume held in a sparse map of 512-byte sectors, so a 33 MB FAT32 image
/// costs only the sectors a test actually touches.
///
/// The hostile-input tests need dozens of independently corrupted copies of a
/// valid volume, and a valid FAT32 volume cannot be smaller than
/// `MIN_FAT32_CLUSTERS` clusters. Materialising each one would be gigabytes of
/// zeroes.
#[derive(Clone)]
pub struct SparseDevice {
    sectors: HashMap<u64, [u8; 512]>,
    capacity: u64,
    pub fail_reads_past: Option<u64>,
    /// Which refusal [`fail_reads_past`](Self::fail_reads_past) and
    /// [`flush_refuses`](Self::flush_refuses) answer with.
    ///
    /// `IoError` is two variants and only one of them is a fact about the
    /// device, so a fake that could only ever say `Device` could not exercise
    /// the other half of the mapping at all.
    pub refusal: IoError,
    /// Whether [`BlockAccess::flush`] refuses, which is the one call
    /// `Fat32::sync` makes into the device after the FSInfo write.
    pub flush_refuses: bool,
    /// The volume's own declared size, when the device is deliberately larger
    /// than it — a partition with slack, or an adapter that reports the whole
    /// device. Requests past it are still served, and counted.
    pub volume_bytes: Option<u64>,
    /// Requests the crate made outside the volume. Must stay zero: the
    /// `BlockAccess` contract says the crate never asks for bytes it has not
    /// bounded, and an adapter is entitled to build on that. Counting rather
    /// than refusing is the point — refusing would let a crate that asks look
    /// identical to one that does not.
    pub out_of_volume: u32,
}

impl SparseDevice {
    pub fn from_prefix(prefix: &[u8], capacity: u64) -> SparseDevice {
        let mut dev = SparseDevice {
            sectors: HashMap::new(),
            capacity,
            fail_reads_past: None,
            refusal: IoError::Device,
            flush_refuses: false,
            volume_bytes: None,
            out_of_volume: 0,
        };
        for (i, chunk) in prefix.chunks(512).enumerate() {
            let mut s = [0u8; 512];
            s[..chunk.len()].copy_from_slice(chunk);
            dev.sectors.insert(i as u64, s);
        }
        dev
    }

    pub fn peek(&self, offset: u64, len: usize) -> Vec<u8> {
        let mut out = vec![0u8; len];
        self.read_bytes(offset, &mut out);
        out
    }

    pub fn poke(&mut self, offset: u64, bytes: &[u8]) {
        let mut done = 0usize;
        while done < bytes.len() {
            let pos = offset + done as u64;
            let sector = pos / 512;
            let within = (pos % 512) as usize;
            let n = (512 - within).min(bytes.len() - done);
            let slot = self.sectors.entry(sector).or_insert([0u8; 512]);
            slot[within..within + n].copy_from_slice(&bytes[done..done + n]);
            done += n;
        }
    }

    fn note_range(&mut self, end: u64) {
        if self.volume_bytes.is_some_and(|v| end > v) {
            self.out_of_volume += 1;
        }
    }

    fn read_bytes(&self, offset: u64, buf: &mut [u8]) {
        let mut done = 0usize;
        while done < buf.len() {
            let pos = offset + done as u64;
            let sector = pos / 512;
            let within = (pos % 512) as usize;
            let n = (512 - within).min(buf.len() - done);
            match self.sectors.get(&sector) {
                Some(s) => buf[done..done + n].copy_from_slice(&s[within..within + n]),
                None => buf[done..done + n].fill(0),
            }
            done += n;
        }
    }
}

impl BlockAccess for SparseDevice {
    fn capacity(&self) -> u64 {
        self.capacity
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), IoError> {
        let end = offset.checked_add(buf.len() as u64).ok_or(IoError::Device)?;
        if end > self.capacity {
            return Err(IoError::Device);
        }
        if let Some(limit) = self.fail_reads_past {
            if end > limit {
                return Err(self.refusal);
            }
        }
        self.note_range(end);
        self.read_bytes(offset, buf);
        Ok(())
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> Result<(), IoError> {
        let end = offset.checked_add(buf.len() as u64).ok_or(IoError::Device)?;
        if end > self.capacity {
            return Err(IoError::Device);
        }
        self.note_range(end);
        self.poke(offset, buf);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), IoError> {
        if self.flush_refuses {
            return Err(self.refusal);
        }
        Ok(())
    }
}

/// A device that refuses the next write touching a chosen byte range, once, and
/// passes everything else straight through to `inner`.
///
/// It stages the one failure QEMU will not produce and no host option can inject
/// (`kernel/src/block.rs`'s `OPERATION` budget note): a mirror write taken and
/// the active-FAT write of the *same* `set_fat_entry` refused on the device's
/// own budget after the first is durable. `set_fat_entry` writes the active FAT
/// last for exactly this, so armed on the active FAT's region this refuses that
/// second write and leaves the split a re-drive must heal.
pub struct RefuseOnceInRange<D> {
    inner: D,
    /// `Some(lo, hi)` while armed. A write overlapping `[lo, hi)` is refused and
    /// disarms this, so only one write is lost — the way one expired budget
    /// refuses one operation and the next runs on a fresh one.
    armed: Option<(u64, u64)>,
    refusal: IoError,
}

impl<D: BlockAccess> RefuseOnceInRange<D> {
    /// Disarmed: wrap now, arm with [`Self::arm`] once the setup writes that
    /// must succeed (the create, any directory growth) are done.
    pub fn new(inner: D, refusal: IoError) -> RefuseOnceInRange<D> {
        RefuseOnceInRange { inner, armed: None, refusal }
    }

    pub fn arm(&mut self, range: (u64, u64)) {
        self.armed = Some(range);
    }
}

impl<D: BlockAccess> BlockAccess for RefuseOnceInRange<D> {
    fn capacity(&self) -> u64 {
        self.inner.capacity()
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), IoError> {
        self.inner.read_at(offset, buf)
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> Result<(), IoError> {
        if let Some((lo, hi)) = self.armed {
            let end = offset + buf.len() as u64;
            if offset < hi && end > lo {
                self.armed = None;
                return Err(self.refusal);
            }
        }
        self.inner.write_at(offset, buf)
    }

    fn flush(&mut self) -> Result<(), IoError> {
        self.inner.flush()
    }
}

/// `kernel/src/fat32_adapter.rs`'s `BLOCK`.
pub const ADAPTER_BLOCK: usize = 4096;

/// `kernel/src/fat32_adapter.rs`'s `RESIDENT_BLOCKS`.
pub const ADAPTER_RESIDENT_BLOCKS: usize = 8;

/// Where a refused block write got to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Landing {
    /// The write never reached the medium.
    NotReached,
    /// The write reached the medium and was answered as refused anyway.
    Reached,
}

/// A medium in memory behind `FatDevice`'s resident blocks, served and written
/// the way `FatDevice::read_at` and `FatDevice::write_at` serve and write it.
///
/// A request moves in 4 KiB blocks. A whole block bypasses the resident copy,
/// and a whole-block write forgets it. A partial block is read through `load`,
/// which serves the resident copy and fills it from the medium on a miss, and
/// is written read-modify-write over that copy: the whole block reaches the
/// medium, carrying the resident neighbours with it, and is retained only once
/// the write answers. Eight blocks are resident, evicted round-robin.
///
/// A refusal falls on a request's first block write, and the request stops
/// there as the adapter's `?` stops it. A request refused at a later block, the
/// earlier ones written and retained, is not generated.
pub struct AdapterCache {
    /// Padded to whole blocks, as the partition under a volume is.
    medium: Vec<u8>,
    /// The volume's own length.
    len: usize,
    resident: Vec<u8>,
    tags: [Option<usize>; ADAPTER_RESIDENT_BLOCKS],
    next_victim: usize,
    scratch: Vec<u8>,
}

impl AdapterCache {
    pub fn new(mut medium: Vec<u8>) -> AdapterCache {
        let len = medium.len();
        medium.resize(len.next_multiple_of(ADAPTER_BLOCK), 0);
        AdapterCache {
            medium,
            len,
            resident: vec![0u8; ADAPTER_RESIDENT_BLOCKS * ADAPTER_BLOCK],
            tags: [None; ADAPTER_RESIDENT_BLOCKS],
            next_victim: 0,
            scratch: vec![0u8; ADAPTER_BLOCK],
        }
    }

    pub fn len(&self) -> u64 {
        self.len as u64
    }

    /// The volume as the medium holds it, which is what a judge reads.
    pub fn medium(&self) -> &[u8] {
        &self.medium[..self.len]
    }

    pub fn into_medium(mut self) -> Vec<u8> {
        self.medium.truncate(self.len);
        self.medium
    }

    /// Change the medium behind the adapter's back, dropping the blocks it
    /// touches from the resident copy.
    pub fn poke(&mut self, offset: usize, bytes: &[u8]) {
        self.medium[offset..offset + bytes.len()].copy_from_slice(bytes);
        let first = offset / ADAPTER_BLOCK;
        let last = (offset + bytes.len()).div_ceil(ADAPTER_BLOCK);
        self.forget(first, last - first);
    }

    fn slot_of(&self, block: usize) -> Option<usize> {
        self.tags.iter().position(|&t| t == Some(block))
    }

    fn load(&mut self, block: usize) {
        if let Some(slot) = self.slot_of(block) {
            let at = slot * ADAPTER_BLOCK;
            self.scratch.copy_from_slice(&self.resident[at..at + ADAPTER_BLOCK]);
            return;
        }
        let at = block * ADAPTER_BLOCK;
        self.scratch.copy_from_slice(&self.medium[at..at + ADAPTER_BLOCK]);
        self.retain(block);
    }

    fn retain(&mut self, block: usize) {
        let slot = self.slot_of(block).unwrap_or_else(|| {
            let s = self.next_victim;
            self.next_victim = (s + 1) % ADAPTER_RESIDENT_BLOCKS;
            s
        });
        let at = slot * ADAPTER_BLOCK;
        self.resident[at..at + ADAPTER_BLOCK].copy_from_slice(&self.scratch);
        self.tags[slot] = Some(block);
    }

    fn forget(&mut self, first: usize, count: usize) {
        for tag in &mut self.tags {
            if tag.is_some_and(|b| b >= first && b < first + count) {
                *tag = None;
            }
        }
    }

    /// `FatDevice::read_at`, over a range the caller has bounded.
    pub fn read(&mut self, offset: u64, buf: &mut [u8]) {
        let base = offset as usize;
        let mut done = 0usize;
        while done < buf.len() {
            let at = base + done;
            let block = at / ADAPTER_BLOCK;
            let within = at % ADAPTER_BLOCK;
            let left = buf.len() - done;
            if within == 0 && left >= ADAPTER_BLOCK {
                let end = done + left / ADAPTER_BLOCK * ADAPTER_BLOCK;
                buf[done..end].copy_from_slice(&self.medium[at..at + (end - done)]);
                done = end;
            } else {
                let n = (ADAPTER_BLOCK - within).min(left);
                self.load(block);
                buf[done..done + n].copy_from_slice(&self.scratch[within..within + n]);
                done += n;
            }
        }
    }

    /// `FatDevice::write_at`, over a range the caller has bounded: every block
    /// written, or with `refusal` the first block write refused where it got
    /// to and nothing after it issued.
    pub fn write(&mut self, offset: u64, buf: &[u8], refusal: Option<Landing>) {
        let base = offset as usize;
        let reaches = refusal != Some(Landing::NotReached);
        let mut done = 0usize;
        while done < buf.len() {
            let at = base + done;
            let block = at / ADAPTER_BLOCK;
            let within = at % ADAPTER_BLOCK;
            let left = buf.len() - done;
            if within == 0 && left >= ADAPTER_BLOCK {
                let count = left / ADAPTER_BLOCK;
                self.forget(block, count);
                let issued = if refusal.is_some() { ADAPTER_BLOCK } else { count * ADAPTER_BLOCK };
                if reaches {
                    self.medium[at..at + issued].copy_from_slice(&buf[done..done + issued]);
                }
                done += count * ADAPTER_BLOCK;
            } else {
                let n = (ADAPTER_BLOCK - within).min(left);
                self.load(block);
                self.scratch[within..within + n].copy_from_slice(&buf[done..done + n]);
                if reaches {
                    let start = block * ADAPTER_BLOCK;
                    self.medium[start..start + ADAPTER_BLOCK].copy_from_slice(&self.scratch);
                }
                if refusal.is_none() {
                    self.retain(block);
                }
                done += n;
            }
            if refusal.is_some() {
                return;
            }
        }
    }
}

// ----------------------------------------------------------------- images

pub struct Image {
    pub path: PathBuf,
    label: String,
}

impl Image {
    /// A sparse file of `bytes`, formatted by `newfs_msdos -F 32` through a
    /// `hdiutil` device node.
    ///
    /// `newfs_msdos` refuses a plain file — it wants a device to ask for the
    /// partition offset — so the file is attached first. The file stays sparse
    /// throughout, which is what makes a 300 MB volume with 4 KiB clusters
    /// affordable.
    pub fn new(name: &str, bytes: u64, sectors_per_cluster: u32) -> Image {
        let seq = LABEL_SEQ.fetch_add(1, Ordering::Relaxed);
        let label = format!("TF{:09}", (std::process::id() * 1000 + seq) % 1_000_000_000);
        let path = scratch_root().join(format!("{name}-{label}.img"));
        let _ = std::fs::remove_file(&path);
        let file = File::create(&path).expect("create image");
        file.set_len(bytes).expect("size image");
        drop(file);

        let guard = HDIUTIL.lock().unwrap_or_else(|e| e.into_inner());
        let dev = attach(&path, false).0;
        let (ok, out) = run(Command::new("/sbin/newfs_msdos").args([
            "-F",
            "32",
            "-S",
            "512",
            "-c",
            &sectors_per_cluster.to_string(),
            "-v",
            &label,
            &dev,
        ]));
        detach(&dev);
        drop(guard);
        assert!(ok, "newfs_msdos failed on {}:\n{out}", path.display());

        Image { path, label }
    }

    pub fn size(&self) -> u64 {
        std::fs::metadata(&self.path).expect("stat").len()
    }

    pub fn device(&self) -> BlockyFile {
        BlockyFile::open(&self.path, 4096)
    }

    pub fn bytes(&self, len: usize) -> Vec<u8> {
        let mut f = File::open(&self.path).expect("open");
        let mut buf = vec![0u8; len];
        f.read_exact(&mut buf).expect("read prefix");
        buf
    }

    /// [`assert_fats_agree`] on the image file, which a [`BlockyFile`] writes
    /// through with no copy of its own.
    pub fn assert_fats_agree(&self, g: &toyos_fat32::Geometry) {
        assert_fats_agree(g, &self.bytes(g.fat_base_offset(g.num_fats) as usize));
    }

    /// Attach and mount, run `f`, remove the files macOS leaves behind, and
    /// detach.
    pub fn with_mount<T>(&self, f: impl FnOnce(&Path) -> T) -> T {
        let guard = HDIUTIL.lock().unwrap_or_else(|e| e.into_inner());
        let (dev, mount) = attach(&self.path, true);
        let mount = mount.unwrap_or_else(|| panic!("{} did not mount", self.path.display()));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(Path::new(&mount))));
        // AppleDouble sidecars and Spotlight state are macOS's, not the test's.
        delete_apple_double_sidecars(Path::new(&mount));
        for junk in [".fseventsd", ".Spotlight-V100", ".Trashes", ".TemporaryItems"] {
            let _ = std::fs::remove_dir_all(Path::new(&mount).join(junk));
        }
        detach(&dev);
        drop(guard);
        match result {
            Ok(v) => v,
            Err(p) => std::panic::resume_unwind(p),
        }
    }

    /// Assert the volume checker finds nothing to complain about.
    ///
    /// Silence is the whole gate: [`toyos_fat32_check::check`] answers with the
    /// list of invariants the volume breaks, so there is no exit code to
    /// misread and no summary line to filter out.
    pub fn fsck(&self) {
        let bytes = std::fs::read(&self.path).expect("read the volume back");
        let complaints = toyos_fat32_check::check(&bytes);
        assert!(
            complaints.is_empty(),
            "the volume checker is not happy with {}:\n{}",
            self.path.display(),
            toyos_fat32_check::describe(&complaints)
        );
    }
}

impl Drop for Image {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn attach(path: &Path, mount: bool) -> (String, Option<String>) {
    let mut cmd = Command::new("/usr/bin/hdiutil");
    cmd.args(["attach", "-imagekey", "diskimage-class=CRawDiskImage", "-nobrowse"]);
    if !mount {
        cmd.arg("-nomount");
    }
    cmd.arg(path);
    let (ok, out) = run(&mut cmd);
    assert!(ok, "hdiutil attach failed for {}:\n{out}", path.display());

    let line = out.lines().find(|l| l.starts_with("/dev/")).unwrap_or_else(|| {
        panic!("no device node in hdiutil output for {}:\n{out}", path.display())
    });
    let mut parts = line.split('\t').map(str::trim);
    let dev = parts.next().unwrap_or_default().to_string();
    let mount = line.split('\t').map(str::trim).find(|p| p.starts_with("/Volumes/")).map(String::from);
    (dev, mount)
}

fn detach(dev: &str) {
    for _ in 0..10 {
        let (ok, _) = run(Command::new("/usr/bin/hdiutil").args(["detach", dev]));
        if ok {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    let (ok, out) = run(Command::new("/usr/bin/hdiutil").args(["detach", "-force", dev]));
    assert!(ok, "could not detach {dev}:\n{out}");
}

// ------------------------------------------------------------- assertions

/// The error a mount was supposed to produce.
///
/// A free function rather than `unwrap_err`, because that would need `Debug`
/// on the whole filesystem — and a `Debug` on `Fat32` exists only to serve
/// this line, which is not a reason for a public trait impl.
pub fn mount_err<D: BlockAccess>(dev: D) -> toyos_fat32::Error {
    match toyos_fat32::Fat32::mount(dev) {
        Ok(_) => panic!("mount succeeded on a volume that should have been refused"),
        Err(e) => e,
    }
}

pub fn sorted_walk<D: BlockAccess>(fs: &mut toyos_fat32::Fat32<D>) -> Vec<(String, u64)> {
    let mut v = fs.walk("", 4096).expect("walk");
    v.sort();
    v
}

/// What `walk` must return for staged `files`: every file with its length,
/// plus each directory on the way to one as a trailing-slash entry of size 0.
pub fn walk_expectation(files: &[(String, Vec<u8>)]) -> Vec<(String, u64)> {
    let mut want: Vec<(String, u64)> =
        files.iter().map(|(n, d)| (n.clone(), d.len() as u64)).collect();
    let mut dirs = std::collections::BTreeSet::new();
    for (name, _) in files {
        let mut at = 0;
        while let Some(pos) = name[at..].find('/') {
            at += pos + 1;
            dirs.insert(name[..at].to_string());
        }
    }
    want.extend(dirs.into_iter().map(|d| (d, 0)));
    want.sort();
    want
}

/// Read a whole file through the crate.
pub fn read_all<D: BlockAccess>(fs: &mut toyos_fat32::Fat32<D>, path: &str) -> Vec<u8> {
    let mut f = fs.open(path).unwrap_or_else(|e| panic!("open {path}: {e}"));
    let mut out = vec![0u8; f.len() as usize];
    let n = fs.read(&mut f, 0, &mut out).unwrap_or_else(|e| panic!("read {path}: {e}"));
    assert_eq!(n, out.len(), "short read of {path}");
    out
}

/// Write a whole file through the crate, creating it.
pub fn write_new<D: BlockAccess>(
    fs: &mut toyos_fat32::Fat32<D>,
    path: &str,
    data: &[u8],
    time: toyos_fat32::FatTime,
) {
    let mut f = fs.create(path, time).unwrap_or_else(|e| panic!("create {path}: {e}"));
    fs.write(&mut f, 0, data).unwrap_or_else(|e| panic!("write {path}: {e}"));
    fs.flush_meta(&mut f, time).unwrap_or_else(|e| panic!("flush {path}: {e}"));
}

/// Assert every FAT holds the same bytes on `medium`, the volume from its
/// first byte through at least its last FAT.
///
/// The invariant a mount cannot see, because it reads the active copy only: a
/// driver that updates FAT 0 and leaves FAT 1 behind reads back correctly until
/// something consults the mirror. `fsck_msdos` did not compare the copies
/// either, which is how breaking `Geometry::fat_mirrors` on purpose went
/// unnoticed. [`Image::fsck`]'s checker compares them off the raw volume; this
/// asks the same question while the filesystem is still mounted, and the
/// answer names the byte.
pub fn assert_fats_agree(g: &toyos_fat32::Geometry, medium: &[u8]) {
    assert!(g.num_fats >= 2, "volume has one FAT, so this proves nothing");
    let used = (((g.cluster_count as u64 + 2) * 4).div_ceil(g.bytes_per_sector as u64)
        * g.bytes_per_sector as u64) as usize;
    let fat = |n: u32| &medium[g.fat_base_offset(n) as usize..][..used];
    for mirror in 1..g.num_fats {
        let diff = fat(0).iter().zip(fat(mirror)).position(|(x, y)| x != y);
        assert!(diff.is_none(), "FAT {mirror} differs from FAT 0 at byte {}", diff.unwrap_or(0));
    }
}

/// The raw 8.3 name field of every non-free entry in a directory chain.
///
/// Read straight off the device rather than through the crate: the crate
/// reports the *long* name, and duplicate short names are invisible from
/// there — which is why a build that stopped uniquifying them once passed
/// everything, `fsck_msdos` and a real mount included.
pub fn short_names_in<D: BlockAccess>(
    fs: &mut toyos_fat32::Fat32<D>,
    first_cluster: u32,
) -> Vec<[u8; 11]> {
    let g = *fs.geometry();
    let mut out = Vec::new();
    let mut cluster = g.cluster(first_cluster).expect("directory cluster is in the volume");
    let mut buf = vec![0u8; g.bytes_per_cluster() as usize];
    for _ in 0..4096 {
        fs.device().read_at(g.cluster_offset(cluster), &mut buf).expect("read directory cluster");
        for entry in buf.as_chunks::<32>().0 {
            if entry[0] == 0x00 {
                return out;
            }
            let is_lfn = entry[11] & 0x3F == 0x0F;
            let is_label = !is_lfn && entry[11] & 0x08 != 0;
            if entry[0] == 0xE5 || is_lfn || is_label || entry[0] == b'.' {
                continue;
            }
            let mut short = [0u8; 11];
            short.copy_from_slice(&entry[..11]);
            out.push(short);
        }
        let mut link = [0u8; 4];
        fs.device()
            .read_at(g.fat_entry_offset(0, cluster), &mut link)
            .expect("read chain link");
        match g.cluster(u32::from_le_bytes(link) & 0x0FFF_FFFF) {
            Some(next) => cluster = next,
            None => return out,
        }
    }
    out
}

/// Deterministic bytes, so a mismatch says where rather than that.
pub fn pattern(len: usize, seed: u64) -> Vec<u8> {
    let mut s = seed | 1;
    (0..len)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 33) as u8
        })
        .collect()
}
