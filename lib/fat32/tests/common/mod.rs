//! Host-side scaffolding: real FAT32 images made by macOS, devices that carry
//! them, and the volume-checker gate.
//!
//! Nothing here formats a volume. The images are `tests/fixtures/`: macOS's
//! `newfs_msdos` formatted each and its `msdosfs` populated it, once, and
//! `NOTICE` records the commands and the release. They are judged by
//! `toyos-fat32-check`, which is written from the specification and shares no
//! code with this crate — so the ground truth on both sides of every test is
//! something other than the driver under test.
//!
//! Every device here answers each request as it is asked, so a call is driven
//! with [`now`].

#![allow(dead_code)]

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::future::Future;
use std::io::Read;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use toyos_blockdev::{block_on_ready, within, Block, BlockDev, Error as DevError, BLOCK};
use toyos_tmpdir::TempDir;

/// A call on a volume over devices that answer every request as it is asked.
pub fn now<F: Future>(call: F) -> F::Output {
    block_on_ready(call)
}

/// `buf.len()` bytes from `offset` of `dev`, read in the whole blocks they lie
/// on: the device's own bytes, not the crate's reading of them.
pub fn read_at<D: BlockDev>(dev: &D, offset: u64, buf: &mut [u8]) {
    let first = offset / BLOCK as u64;
    let count = (offset + buf.len() as u64).div_ceil(BLOCK as u64) - first;
    let mut blocks = vec![[0u8; BLOCK]; count as usize];
    now(dev.read(first, &mut blocks)).expect("a device read");
    let at = (offset % BLOCK as u64) as usize;
    buf.copy_from_slice(&blocks.as_flattened()[at..at + buf.len()]);
}

// ---------------------------------------------------------------- devices

/// A device backed by a file, in whole blocks: what the crate's own bridge
/// from its sectors to a device's blocks is exercised against.
pub struct BlockyFile {
    file: File,
    blocks: u64,
}

impl BlockyFile {
    pub fn open(path: &Path) -> BlockyFile {
        let file = OpenOptions::new().read(true).write(true).open(path).expect("open image");
        let len = file.metadata().expect("stat image").len();
        assert!(len.is_multiple_of(BLOCK as u64), "{}: {len} bytes, no whole blocks", path.display());
        BlockyFile { file, blocks: len / BLOCK as u64 }
    }
}

impl BlockDev for BlockyFile {
    fn blocks(&self) -> u64 {
        self.blocks
    }

    async fn read(&self, lba: u64, out: &mut [Block]) -> Result<(), DevError> {
        within(lba, out.len(), self.blocks)?;
        self.file.read_exact_at(out.as_flattened_mut(), lba * BLOCK as u64).map_err(|_| DevError::Device)
    }

    async fn write(&self, lba: u64, data: &[Block]) -> Result<(), DevError> {
        within(lba, data.len(), self.blocks)?;
        self.file.write_all_at(data.as_flattened(), lba * BLOCK as u64).map_err(|_| DevError::Device)
    }

    async fn flush(&self) -> Result<(), DevError> {
        self.file.sync_all().map_err(|_| DevError::Device)
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
    sectors: RefCell<HashMap<u64, [u8; 512]>>,
    blocks: u64,
    /// A read whose last byte is past this is refused.
    pub fail_reads_past: Option<u64>,
    /// Whether [`BlockDev::flush`] refuses, which is the one call
    /// `Fat32::sync` makes into the device after the FSInfo write.
    pub flush_refuses: bool,
    /// The volume's own declared size, when the device is deliberately larger
    /// than it — a partition with slack. Requests for blocks wholly past it
    /// are still served, and counted.
    pub volume_bytes: Option<u64>,
    /// Requests the crate made for blocks outside the volume. Must stay zero:
    /// the crate never asks for bytes it has not bounded, and a device is
    /// entitled to build on that. Counting rather than refusing is the point —
    /// refusing would let a crate that asks look identical to one that does
    /// not.
    pub out_of_volume: Cell<u32>,
}

impl SparseDevice {
    /// `prefix` at the start of a device of `bytes`, whole blocks.
    pub fn from_prefix(prefix: &[u8], bytes: u64) -> SparseDevice {
        assert!(bytes.is_multiple_of(BLOCK as u64), "a device of {bytes} bytes, no whole blocks");
        let mut dev = SparseDevice {
            sectors: RefCell::new(HashMap::new()),
            blocks: bytes / BLOCK as u64,
            fail_reads_past: None,
            flush_refuses: false,
            volume_bytes: None,
            out_of_volume: Cell::new(0),
        };
        dev.poke(0, prefix);
        dev
    }

    pub fn peek(&self, offset: u64, len: usize) -> Vec<u8> {
        let mut out = vec![0u8; len];
        self.read_bytes(offset, &mut out);
        out
    }

    pub fn poke(&mut self, offset: u64, bytes: &[u8]) {
        Self::put(self.sectors.get_mut(), offset, bytes);
    }

    fn put(sectors: &mut HashMap<u64, [u8; 512]>, offset: u64, bytes: &[u8]) {
        let mut done = 0usize;
        while done < bytes.len() {
            let pos = offset + done as u64;
            let sector = pos / 512;
            let within = (pos % 512) as usize;
            let n = (512 - within).min(bytes.len() - done);
            let slot = sectors.entry(sector).or_insert([0u8; 512]);
            slot[within..within + n].copy_from_slice(&bytes[done..done + n]);
            done += n;
        }
    }

    /// Count a request for blocks `lba..lba + count` that lies wholly past
    /// the volume.
    fn note_range(&self, lba: u64, count: usize) {
        if count > 0 && self.volume_bytes.is_some_and(|v| lba * BLOCK as u64 >= v.next_multiple_of(BLOCK as u64)) {
            self.out_of_volume.set(self.out_of_volume.get() + 1);
        }
    }

    fn read_bytes(&self, offset: u64, buf: &mut [u8]) {
        let sectors = self.sectors.borrow();
        let mut done = 0usize;
        while done < buf.len() {
            let pos = offset + done as u64;
            let sector = pos / 512;
            let within = (pos % 512) as usize;
            let n = (512 - within).min(buf.len() - done);
            match sectors.get(&sector) {
                Some(s) => buf[done..done + n].copy_from_slice(&s[within..within + n]),
                None => buf[done..done + n].fill(0),
            }
            done += n;
        }
    }
}

impl BlockDev for SparseDevice {
    fn blocks(&self) -> u64 {
        self.blocks
    }

    async fn read(&self, lba: u64, out: &mut [Block]) -> Result<(), DevError> {
        within(lba, out.len(), self.blocks)?;
        let end = (lba + out.len() as u64) * BLOCK as u64;
        if self.fail_reads_past.is_some_and(|limit| end > limit) {
            return Err(DevError::Device);
        }
        self.note_range(lba, out.len());
        self.read_bytes(lba * BLOCK as u64, out.as_flattened_mut());
        Ok(())
    }

    async fn write(&self, lba: u64, data: &[Block]) -> Result<(), DevError> {
        within(lba, data.len(), self.blocks)?;
        self.note_range(lba, data.len());
        Self::put(&mut self.sectors.borrow_mut(), lba * BLOCK as u64, data.as_flattened());
        Ok(())
    }

    async fn flush(&self) -> Result<(), DevError> {
        if self.flush_refuses {
            return Err(DevError::Device);
        }
        Ok(())
    }
}

// ----------------------------------------------------------------- images

pub struct Image {
    pub path: PathBuf,
    /// Where the image file is, gone with it.
    _dir: TempDir,
}

impl Image {
    /// `tests/fixtures/<name>.runs` expanded into a sparse file of this test's
    /// own, which the test may write.
    ///
    /// The form is the image's length, then each run of nonzero 512-byte
    /// sectors as its offset, its length and its bytes, every number a
    /// little-endian `u64`. Everything between two runs is zero, which a sparse
    /// file holds for nothing, so a 300 MB volume with 4 KiB clusters costs what
    /// is on it.
    pub fn fixture(name: &str) -> Image {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/{name}.runs"));
        let runs = std::fs::read(&source).unwrap_or_else(|e| panic!("{}: {e}", source.display()));
        let word = |at: usize| u64::from_le_bytes(runs[at..at + 8].try_into().expect("eight bytes"));
        let dir = TempDir::new(&format!("fat32-{name}"));
        let path = dir.join(format!("{name}.img"));
        let file = File::create(&path).expect("create image");
        file.set_len(word(0)).expect("size image");
        let mut at = 8;
        while at < runs.len() {
            let (offset, len) = (word(at), word(at + 8) as usize);
            assert!(offset + len as u64 <= word(0), "{}: a run at {offset} ends past the image", source.display());
            file.write_all_at(&runs[at + 16..at + 16 + len], offset).expect("write a run");
            at += 16 + len;
        }
        Image { path, _dir: dir }
    }

    pub fn size(&self) -> u64 {
        std::fs::metadata(&self.path).expect("stat").len()
    }

    pub fn device(&self) -> BlockyFile {
        BlockyFile::open(&self.path)
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

// ------------------------------------------------------------- assertions

/// The error a mount was supposed to produce.
///
/// A free function rather than `unwrap_err`, because that would need `Debug`
/// on the whole filesystem — and a `Debug` on `Fat32` exists only to serve
/// this line, which is not a reason for a public trait impl.
pub fn mount_err<D: BlockDev>(dev: D) -> toyos_fat32::Error {
    match now(toyos_fat32::Fat32::mount(dev)) {
        Ok(_) => panic!("mount succeeded on a volume that should have been refused"),
        Err(e) => e,
    }
}

pub fn sorted_walk<D: BlockDev>(fs: &mut toyos_fat32::Fat32<D>) -> Vec<(String, u64)> {
    let mut v = now(fs.walk("", 4096)).expect("walk");
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
pub fn read_all<D: BlockDev>(fs: &mut toyos_fat32::Fat32<D>, path: &str) -> Vec<u8> {
    let mut f = now(fs.open(path)).unwrap_or_else(|e| panic!("open {path}: {e}"));
    let mut out = vec![0u8; f.len() as usize];
    let n = now(fs.read(&mut f, 0, &mut out)).unwrap_or_else(|e| panic!("read {path}: {e}"));
    assert_eq!(n, out.len(), "short read of {path}");
    out
}

/// Write a whole file through the crate, creating it.
pub fn write_new<D: BlockDev>(
    fs: &mut toyos_fat32::Fat32<D>,
    path: &str,
    data: &[u8],
    time: toyos_fat32::FatTime,
) {
    let mut f = now(fs.create(path, time)).unwrap_or_else(|e| panic!("create {path}: {e}"));
    now(fs.write(&mut f, 0, data)).unwrap_or_else(|e| panic!("write {path}: {e}"));
    now(fs.flush_meta(&mut f, time)).unwrap_or_else(|e| panic!("flush {path}: {e}"));
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
pub fn short_names_in<D: BlockDev>(
    fs: &mut toyos_fat32::Fat32<D>,
    first_cluster: u32,
) -> Vec<[u8; 11]> {
    let g = *fs.geometry();
    let mut out = Vec::new();
    let mut cluster = g.cluster(first_cluster).expect("directory cluster is in the volume");
    let mut buf = vec![0u8; g.bytes_per_cluster() as usize];
    for _ in 0..4096 {
        read_at(fs.device(), g.cluster_offset(cluster), &mut buf);
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
        read_at(fs.device(), g.fat_entry_offset(0, cluster), &mut link);
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
