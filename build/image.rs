use std::collections::BTreeMap;
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::path::Path;

use bcachefs::engine::{self, Meta, NodeRef, Tree};

use crate::arch::Arch;
use toyos_blockdev::{block_on_ready, Bytes, ReadOnlyBytes, BLOCK};
use toyos_crypto::Random;
use toyos_fat32::{Fat32, FatTime};
use toyos_gpt::edit::{Geometry, Slot, Step, Table};
use toyos_gpt::Guid;

/// The image that goes on the ROOT partition: a bcachefs volume holding
/// `files` and `symlinks`, every directory their paths name, and nothing
/// free.
///
/// **A function of the set, not of the order or the clock.** Paths are
/// sorted, so inode numbers follow sorted path order from 2^31; every
/// identifier, seed and node sequence mkfs draws comes from a digest of the
/// contents ([`Drawn`]); every time is 0. Two builds of one tree are one
/// image, and the volume's UUID, the name the file server serving it says
/// it by (`toyos_osrelease::ROOT_SAID`), names that tree.
pub fn create_root_image(files: &[(String, Vec<u8>)], symlinks: &[(String, String)], quiet: bool) -> Vec<u8> {
    let mut files: Vec<&(String, Vec<u8>)> = files.iter().collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut symlinks: Vec<&(String, String)> = symlinks.iter().collect();
    symlinks.sort_by(|a, b| a.0.cmp(&b.0));
    let mut random = Drawn::from(root_digest(&files, &symlinks));

    let mut entries: BTreeMap<&str, RootEntry<'_>> = BTreeMap::new();
    let entered = files.iter().map(|(name, data)| (name.as_str(), RootEntry::File(data)));
    for (path, entry) in entered.chain(symlinks.iter().map(|(name, target)| (name.as_str(), RootEntry::Symlink(target)))) {
        if entries.insert(path, entry).is_some() {
            panic!("root: '{path}' is given twice, or is a directory another path names");
        }
        for (at, _) in path.match_indices('/') {
            match entries.entry(&path[..at]).or_insert(RootEntry::Dir) {
                RootEntry::Dir => {}
                _ => panic!("root: '{}' is a file and the directory '{path}' is under", &path[..at]),
            }
        }
    }

    let mut tree = Tree::new();
    let mut dirs: BTreeMap<&str, NodeRef> = BTreeMap::from([("", Tree::ROOT)]);
    for (&path, entry) in &entries {
        let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
        let parent = dirs[parent];
        let added = match entry {
            RootEntry::Dir => tree.dir(parent, name.as_bytes(), ROOT_DIR).map(|dir| {
                dirs.insert(path, dir);
            }),
            RootEntry::File(data) => {
                if !quiet {
                    eprintln!("root: adding '{path}' ({} bytes)", data.len());
                }
                tree.file(parent, name.as_bytes(), data, ROOT_FILE).map(drop)
            }
            RootEntry::Symlink(target) => {
                if !quiet {
                    eprintln!("root: symlink '{path}' -> '{target}'");
                }
                tree.symlink(parent, name.as_bytes(), target.as_bytes(), ROOT_SYMLINK).map(drop)
            }
        };
        added.unwrap_or_else(|e| panic!("root: '{path}' cannot be put on ROOT: {e:?}"));
    }

    let options = engine::Options { label: b"ROOT", time: 0, encryption: None };
    let blocks = engine::fit(&options, &tree, &random).unwrap_or_else(|e| panic!("root: no size holds these files: {e:?}"));
    let device = Bytes::new(vec![0u8; usize::try_from(blocks).expect("a ROOT this host can hold") * BLOCK]);
    block_on_ready(engine::mkfs(&device, &options, &tree, &mut random))
        .unwrap_or_else(|e| panic!("root: mkfs at the size it fits refused: {e:?}"));
    device.into_inner()
}

/// What one path on ROOT is.
enum RootEntry<'a> {
    Dir,
    File(&'a [u8]),
    Symlink(&'a str),
}

/// Owner and permissions of ROOT's directories, files and symlinks: root's,
/// and read-only to everyone else, whatever the build host's were.
const ROOT_DIR: Meta = Meta { perm: 0o755, uid: 0, gid: 0 };
const ROOT_FILE: Meta = Meta { perm: 0o644, uid: 0, gid: 0 };
const ROOT_SYMLINK: Meta = Meta { perm: 0o777, uid: 0, gid: 0 };

/// A digest of exactly this set of files and symlinks, in the order
/// [`create_root_image`] sorted them into. Lengths go into the digest beside
/// the bytes, so no two entries can run together into an input a different
/// split would also produce.
fn root_digest(files: &[&(String, Vec<u8>)], symlinks: &[&(String, String)]) -> [u8; 32] {
    let mut hasher = toyos_crypto::hash::Sha256::new();
    let mut field = |bytes: &[u8]| {
        hasher.update(&(bytes.len() as u64).to_le_bytes()).expect(crate::SHA256_BOUND);
        hasher.update(bytes).expect(crate::SHA256_BOUND);
    };
    for (name, data) in files {
        field(name.as_bytes());
        field(data);
    }
    for (name, target) in symlinks {
        field(name.as_bytes());
        field(target.as_bytes());
    }
    hasher.finalize()
}

/// The bytes mkfs draws for ROOT, a function of [`root_digest`] alone:
/// SHA-256 of the digest and a block counter, in counter order.
#[derive(Clone)]
struct Drawn {
    digest: [u8; 32],
    counter: u64,
    block: [u8; 32],
    used: usize,
}

impl From<[u8; 32]> for Drawn {
    fn from(digest: [u8; 32]) -> Self {
        Self { digest, counter: 0, block: [0; 32], used: 32 }
    }
}

impl Random for Drawn {
    fn fill(&mut self, out: &mut [u8]) -> Result<(), toyos_crypto::Error> {
        for byte in out {
            if self.used == self.block.len() {
                self.block = crate::sha256(&[&self.digest[..], &self.counter.to_le_bytes()].concat());
                self.counter += 1;
                self.used = 0;
            }
            *byte = self.block[self.used];
            self.used += 1;
        }
        Ok(())
    }
}

/// ROOT's name, as the file server serving it says it: its volume's UUID,
/// 32 hex digits.
pub fn root_uuid_of(volume: &[u8]) -> Result<String, String> {
    let root = mount_root(volume)?;
    Ok(root.superblock().user_uuid().iter().map(|b| format!("{b:02x}")).collect())
}

/// The ROOT volume `bytes`, opened by the reader the loader and the file
/// server read it with.
fn mount_root(bytes: &[u8]) -> Result<engine::Volume<ReadOnlyBytes<'_>>, String> {
    let device = ReadOnlyBytes::new(bytes).ok_or_else(|| format!("a {}-byte ROOT, which is no whole {BLOCK}-byte blocks", bytes.len()))?;
    block_on_ready(engine::Volume::open(device)).map_err(|e| format!("ROOT does not mount: {e:?}"))
}

/// The file `path` on the ROOT volume `bytes`, following no symlink.
pub fn root_file(bytes: &[u8], path: &str) -> Result<Vec<u8>, String> {
    engine::read_file(bytes, path.as_bytes()).map_err(|e| format!("ROOT: reading {path}: {e:?}"))
}

/// What an image is signed with: the key, and the monotonic version the
/// signed header names.
#[derive(Clone, Copy)]
pub struct Signing<'a> {
    pub key: &'a crate::signing::Key,
    pub version: u64,
}

/// The version a build that names none signs its image with: its Unix time in
/// seconds, which the owner's one Mac only moves forward.
pub fn version_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("this host's clock is past 1970")
        .as_secs()
}

/// An image's second slot: empty, with room for an update's ROOT of up to
/// `root_bytes`. An image without one is a machine that cannot update itself,
/// which is every QEMU guest but the ones whose subject is the update.
#[derive(Clone, Copy, Debug)]
pub struct SecondSlot {
    pub root_bytes: u64,
}

/// What one signed image is made of but its boot parameter: the kernel, the
/// two programs the kernel starts the machine with — the supervisor, and the
/// file server that serves ROOT to it — and ROOT, which carries neither
/// program (`toyos_update::image`).
#[derive(Clone, Copy)]
pub struct Release<'a> {
    pub kernel: &'a [u8],
    pub supervisor: &'a [u8],
    pub fileserver: &'a [u8],
    pub root: &'a [u8],
}

/// The signed header of one image and the boot parameter it names.
struct Signed {
    cmdline: Vec<u8>,
    signed: [u8; toyos_update::image::SIGNED_BYTES],
}

fn signed(release: Release<'_>, params: &str, signing: Signing<'_>) -> Signed {
    let cmdline = params.as_bytes().to_vec();
    let header = toyos_update::image::Header::of(signing.version, &sections(release, &cmdline));
    Signed { signed: signing.key.sign(&header), cmdline }
}

/// `release` and `cmdline` as the signed header names them.
fn sections<'a>(release: Release<'a>, cmdline: &'a [u8]) -> toyos_update::image::Sections<'a> {
    let Release { kernel, supervisor, fileserver, root } = release;
    toyos_update::image::Sections { kernel, cmdline, supervisor, fileserver, root }
}

/// The image `ssh <machine> update` takes on its standard input: the signed
/// header, then every section in the order the header names them
/// (`toyos_update::image`), ROOT last.
pub fn update_image(release: Release<'_>, params: &str, signing: Signing<'_>) -> Vec<u8> {
    let s = signed(release, params, signing);
    let mut out = s.signed.to_vec();
    for section in sections(release, &s.cmdline).all() {
        out.extend_from_slice(section);
    }
    out
}

/// Takes the artifacts as bytes rather than reading them: the caller stages them
/// under a build-key-derived name first, because cargo's own path is shared by
/// every config and is overwritten by any concurrent build (see `build.rs`).
///
/// The disk: the ESP (the loader and the log partition's name), the slot
/// table, the log partition — third, where the metal loop finds it — and then
/// slot A's FAT and ROOT, marked, and slot B's where `second` asks for one.
pub fn create_boot_image(
    arch: Arch,
    bl_bytes: &[u8],
    release: Release<'_>,
    params: &str,
    signing: Signing<'_>,
    second: Option<SecondSlot>,
) -> Vec<u8> {
    // Drawn here and written twice: into the GPT entry that *is* the log
    // partition, and into a file on the ESP that the bootloader hands the
    // kernel. The kernel is given the partition by name; nothing anywhere goes
    // looking for one by type or by format. Every slot partition is named the
    // same way, by the slot table.
    let (log_guid, esp_guid, table_guid) = (random_guid(), random_guid(), random_guid());
    let a = (random_guid(), random_guid());
    let b = second.map(|room| (random_guid(), random_guid(), room));

    let s = signed(release, params, signing);
    // Room for the slot's files twice over, so an update can write new ones
    // into a slot's volume before the old ones are gone.
    let files = release.kernel.len() + release.supervisor.len() + release.fileserver.len();
    let slot_bytes = round_up_sectors(((files * 2 + ESP_FREE_BYTES) * 64 / 63).max(FAT32_MIN_BYTES));
    let a_boot = create_slot_volume(Some(SlotFiles { release, cmdline: &s.cmdline, signed: &s.signed }), slot_bytes);
    let slot = |(boot, root): (Guid, Guid), version| toyos_update::slots::Slot { boot: boot.0, root: root.0, version };
    let table = toyos_update::slots::Table {
        sequence: 1,
        marked: toyos_update::slots::Which::A,
        slots: [Some(slot(a, signing.version)), b.map(|(boot, root, _)| slot((boot, root), 0))],
    };
    let mut table_volume = vec![0u8; PARTITION_ALIGN];
    table_volume[..toyos_update::slots::BLOCK].copy_from_slice(&table.encode());

    let mut parts = vec![
        Part::full("ESP", "EFI System", Guid::EFI_SYSTEM, esp_guid, create_esp_volume(arch, bl_bytes, log_guid), Some(Volume::Fat32)),
        Part::full("slot table", "ToyOS slots", Guid::TOYOS_SLOTS, table_guid, table_volume, None),
        // Microsoft Basic Data, and that type is the whole reason this is a
        // partition of its own: macOS never auto-mounts an EFI-typed partition
        // and this host refuses even a manual non-root mount of one, so a log on
        // the ESP is unreachable without the admin account. This type mounts in
        // Finder, in Windows and in Linux on plug-in, with nothing configured.
        Part::full("log partition", "ToyOS log", Guid::MICROSOFT_BASIC, log_guid, create_log_volume(), Some(Volume::Fat32)),
        Part::full("slot A's volume", "ToyOS slot A", Guid::TOYOS_BOOT, a.0, a_boot, Some(Volume::Fat32)),
        Part::full("slot A's ROOT", "ToyOS root A", Guid::TOYOS_ROOT, a.1, release.root.to_vec(), Some(Volume::Root)),
    ];
    if let Some((boot, root, room)) = b {
        parts.push(Part::full("slot B's volume", "ToyOS slot B", Guid::TOYOS_BOOT, boot, create_slot_volume(None, slot_bytes), Some(Volume::Fat32)));
        let len = align_up(room.root_bytes as usize, PARTITION_ALIGN) as u64;
        parts.push(Part { what: "slot B's ROOT", name: "ToyOS root B", kind: Guid::TOYOS_ROOT, guid: root, bytes: Vec::new(), len, judge: None });
    }
    create_gpt_disk(&parts)
}

/// The actuator list an image is armed with, read back off the image.
pub fn params_of(path: &Path) -> Result<Vec<String>, String> {
    let text = cmdline_of(path)?;
    Ok(toyos_abi::boot::tokens(&text).map(str::to_string).collect())
}

/// The whole boot parameter the image's marked slot carries, read back off
/// the image: the slot table through the partition table, then the slot's
/// volume, rather than at the offsets [`create_gpt_disk`] happens to place
/// them at.
fn cmdline_of(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("opening {}: {e}", path.display()))?;
    let table = slot_table_of(&mut file).map_err(|why| format!("{}: {why}", path.display()))?;
    let slot = table.slot(table.marked).expect("a table marks a slot it carries");
    let bytes = read_file_on(&mut file, slot.boot, toyos_update::slots::CMDLINE_FILE)
        .map_err(|why| format!("{}: {why}", path.display()))?;
    String::from_utf8(bytes).map_err(|e| format!("the boot parameter on {} is not text: {e}", path.display()))
}

/// The ROOT of the image's marked slot, read back off the image at `path`:
/// its name, and the file `name` it carries, read with the reader the file
/// server serves it with.
pub fn root_file_on(path: &Path, name: &str) -> Result<(String, Vec<u8>), String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("opening {}: {e}", path.display()))?;
    let table = slot_table_of(&mut file).map_err(|why| format!("{}: {why}", path.display()))?;
    let slot = table.slot(table.marked).expect("a table marks a slot it carries");
    let (start, len) = partition_extent(&mut file, slot.root).map_err(|why| format!("{}: {why}", path.display()))?;
    let len = usize::try_from(len).map_err(|_| format!("a {len}-byte ROOT on {}", path.display()))?;
    let mut volume = vec![0u8; len];
    file.seek(SeekFrom::Start(start))
        .and_then(|_| file.read_exact(&mut volume))
        .map_err(|e| format!("reading {}'s ROOT at byte {start}: {e}", path.display()))?;
    let on = |why: String| format!("{}: {why}", path.display());
    Ok((root_uuid_of(&volume).map_err(on)?, root_file(&volume, name).map_err(on)?))
}

/// The slot table on the disk image `file`, as the loader reads it.
pub fn slot_table_of(file: &mut std::fs::File) -> Result<toyos_update::slots::Table, String> {
    table_on(file).map(|(table, _, _)| table)
}

/// Where the partition `guid` names is on the disk image `file`, in bytes.
pub fn partition_extent(file: &mut std::fs::File, guid: [u8; 16]) -> Result<(u64, u64), String> {
    let mut out = [None; 16];
    let scan = toyos_gpt::list(&mut FileSectors(file), &mut out)
        .map_err(|e| format!("no readable partition table: {e:?}"))?;
    let unique = |entry: &toyos_gpt::Entry| match entry {
        Ok(p) => p.unique_guid(),
        Err(unplaced) => unplaced.unique_guid,
    };
    let found: Vec<&toyos_gpt::Entry> =
        out.iter().flatten().filter(|entry| unique(entry) == toyos_gpt::Guid(guid)).collect();
    match found[..] {
        [Ok(p)] if scan.matched as usize <= out.len() => {
            Ok((p.first_lba() * u64::from(LBA), p.lba_count().get() * u64::from(LBA)))
        }
        [Err(unplaced)] => {
            Err(format!("partition {}: its blocks are no partition: {unplaced:?}", toyos_gpt::Guid(guid)))
        }
        _ => Err(format!(
            "partition {}: the table states it {} time(s) among {} entries",
            toyos_gpt::Guid(guid),
            found.len(),
            scan.matched
        )),
    }
}

/// The file `name` on the FAT partition `guid` names, read with the driver the
/// kernel mounts such a volume with rather than the crate that formatted it —
/// [`populate`]'s argument, in the other direction. The volume alone, and never
/// the whole image: a test image is a quarter of a gigabyte.
pub fn read_file_on(file: &mut std::fs::File, guid: [u8; 16], name: &str) -> Result<Vec<u8>, String> {
    let (start, len) = partition_extent(file, guid)?;
    let len = usize::try_from(len).map_err(|_| format!("a {len}-byte volume"))?;
    let mut volume = vec![0u8; len];
    file.seek(SeekFrom::Start(start))
        .and_then(|_| file.read_exact(&mut volume))
        .map_err(|e| format!("reading the volume at byte {start}: {e}"))?;
    let device = device_of(&mut volume)?;
    block_on_ready(async {
        let mut fs = Fat32::mount(device).await.map_err(|e| format!("the volume does not mount: {e}"))?;
        let mut found = fs.open(name).await.map_err(|e| format!("the volume has no {name}: {e}"))?;
        let mut bytes = vec![0u8; usize::try_from(found.len()).unwrap_or(usize::MAX)];
        fs.read(&mut found, 0, &mut bytes).await.map_err(|e| format!("reading {name}: {e}"))?;
        Ok(bytes)
    })
}

/// Every file in the root of the image's log volume (its one Microsoft basic
/// data partition), by name, read with the driver the kernel mounts it with.
pub fn log_volume_files(file: &mut std::fs::File) -> Result<Vec<(String, Vec<u8>)>, String> {
    let part = only_partition(&mut FileSectors(file), toyos_gpt::Guid::MICROSOFT_BASIC)?;
    let (start, len) = (part.first_lba() * u64::from(LBA), part.lba_count().get() * u64::from(LBA));
    let len = usize::try_from(len).map_err(|_| format!("a {len}-byte volume"))?;
    let mut volume = vec![0u8; len];
    file.seek(SeekFrom::Start(start))
        .and_then(|_| file.read_exact(&mut volume))
        .map_err(|e| format!("reading the log volume at byte {start}: {e}"))?;
    let device = device_of(&mut volume)?;
    block_on_ready(async {
        let mut fs = Fat32::mount(device).await.map_err(|e| format!("the log volume does not mount: {e}"))?;
        let entries = fs.read_dir("/", usize::MAX).await.map_err(|e| format!("listing the log volume: {e}"))?;
        let mut files = Vec::new();
        for entry in entries.into_iter().filter(|e| !e.is_dir) {
            let path = format!("/{}", entry.name);
            let mut found = fs.open(&path).await.map_err(|e| format!("opening {path}: {e}"))?;
            let mut bytes = vec![0u8; usize::try_from(found.len()).unwrap_or(usize::MAX)];
            fs.read(&mut found, 0, &mut bytes).await.map_err(|e| format!("reading {path}: {e}"))?;
            files.push((entry.name, bytes));
        }
        Ok(files)
    })
}

/// A volume read off an image, as the device `toyos-fat32` mounts: whole
/// blocks, which every partition this build writes is ([`PARTITION_ALIGN`]).
fn device_of(volume: &mut [u8]) -> Result<Bytes<&mut [u8]>, String> {
    match volume.len().is_multiple_of(BLOCK) {
        true => Ok(Bytes::new(volume)),
        false => Err(format!("a {}-byte volume, which is no whole {BLOCK}-byte blocks", volume.len())),
    }
}

/// Why a scan's `out[0]` and `matched` count did not pick out exactly one
/// partition, once the table itself was readable.
pub enum OnePartitionError {
    /// The one entry of the wanted type is no partition on this disk.
    Unplaced(toyos_gpt::Stated),
    /// Not exactly one entry carried the wanted type.
    Matched(u32),
}

/// The one partition a [`toyos_gpt::locate_type`] scan found, out of its
/// [`toyos_gpt::TypeScan`] and the `out[0]` slot it filled — the match shared
/// by every caller that owes exactly one partition of a type and nothing else.
pub fn one_partition_of(
    scan: toyos_gpt::TypeScan,
    first: Option<toyos_gpt::Entry>,
) -> Result<toyos_gpt::Partition, OnePartitionError> {
    match (scan.matched, first) {
        (1, Some(Ok(part))) => Ok(part),
        (1, Some(Err(unplaced))) => Err(OnePartitionError::Unplaced(unplaced)),
        (matched, _) => Err(OnePartitionError::Matched(matched)),
    }
}

/// The one partition of type `kind` on `disk`.
pub fn only_partition(disk: &mut dyn toyos_gpt::Sectors, kind: toyos_gpt::Guid) -> Result<toyos_gpt::Partition, String> {
    let mut out = [None; 2];
    let scan = toyos_gpt::locate_type(disk, kind, &mut out)
        .map_err(|e| format!("no readable partition table: {e:?}"))?;
    one_partition_of(scan, out[0]).map_err(|e| match e {
        OnePartitionError::Unplaced(unplaced) => {
            format!("the one entry of type {kind} is no partition: {unplaced:?}")
        }
        OnePartitionError::Matched(n) => format!("{n} partitions of type {kind}, where one is owed"),
    })
}

/// The slot table on the disk image `file`, which copy is current, and where
/// its partition starts.
fn table_on(file: &mut std::fs::File) -> Result<(toyos_update::slots::Table, usize, u64), String> {
    let part = only_partition(&mut FileSectors(file), toyos_gpt::Guid::TOYOS_SLOTS)?;
    let mut copies = [[0u8; toyos_update::slots::BLOCK]; 2];
    let at = part.first_lba() * u64::from(LBA);
    for (i, copy) in copies.iter_mut().enumerate() {
        file.seek(SeekFrom::Start(at + (i * toyos_update::slots::BLOCK) as u64))
            .and_then(|_| file.read_exact(copy))
            .map_err(|e| format!("reading the slot table's copy {i}: {e}"))?;
    }
    let (table, copy) = toyos_update::slots::current([&copies[0], &copies[1]])
        .map_err(|why| format!("the slot table's partition holds {why}"))?;
    Ok((table, copy, at))
}

/// A raw block device rejects a write that is not a whole number of sectors, so
/// an image whose length is not sector-aligned cannot be `dd`'d to a USB stick —
/// the final partial write fails with `EINVAL` and the tail, including the
/// backup GPT, never lands. QEMU reads the image as a file and never noticed.
const SECTOR: usize = 4096;

/// The logical block a GPT on this image is written and read in.
pub const LBA: u32 = 512;

fn round_up_sectors(n: usize) -> usize {
    n.div_ceil(SECTOR) * SECTOR
}

/// Where each partition is made to start.
///
/// A correctness requirement rather than tidiness. Every block service
/// transfers whole 4 KiB blocks and each file server keeps its own cached
/// copies of the blocks it has touched (`system/fileserver`); two partitions
/// sharing one device block would make each other's copies stale with
/// nothing able to notice. 1 MiB rather than the 4096 the kernel needs,
/// because that is what every partitioner uses and what an erase block wants.
const PARTITION_ALIGN: usize = 1024 * 1024;

/// The smallest volume there is a FAT32 for.
///
/// FAT32 *is* the format with at least 65,525 clusters, and [`crate::fatformat`]
/// gives a volume this size 512-byte clusters — so the data area alone is 33.5 MiB
/// before the two FATs and the reserved sectors. Measured at exactly this size:
/// the format succeeds and `fsck_msdos` reports 68,551 free clusters.
const FAT32_MIN_BYTES: usize = 34 * 1024 * 1024;

/// What a guest can write into `/boot` after the three files are on it.
///
/// The `64/63` beside this at the one call site is headroom for the two FAT
/// copies, which cost a byte of table per 64 bytes of volume at the 512-byte
/// clusters [`crate::fatformat`] gives a small one — the worst case, so a volume whose
/// clusters are larger is over-provisioned rather than under. A flat slack
/// leaves a guest whatever the rounding did, and an fsync that fails while the
/// host-side volume reports megabytes free is the symptom of getting it wrong.
const ESP_FREE_BYTES: usize = 4 * 1024 * 1024;

fn align_up(n: usize, to: usize) -> usize {
    n.div_ceil(to) * to
}

/// A FAT volume label: eleven bytes of space-padded OEM text.
///
/// Without one every host calls the volume `NO NAME`, which is what the ESP
/// showed as. The format writes it into both places the format keeps it,
/// the BPB field and a `VOLUME_ID` entry in the root directory, and the mount
/// on macOS is named from it — measured, `/Volumes/TOYOS-LOG`.
fn fat_label(text: &str) -> [u8; 11] {
    let mut label = [b' '; 11];
    assert!(
        text.len() <= label.len(),
        "a FAT volume label is 11 bytes and {text:?} is {}",
        text.len()
    );
    label[..text.len()].copy_from_slice(text.as_bytes());
    label
}

/// An empty FAT32 volume of `bytes`, under `label`.
fn format_fat32(bytes: usize, label: &str) -> Vec<u8> {
    crate::fatformat::format(bytes, fat_label(label))
}

/// When the build ran, which is what the files on the stick are dated.
///
/// A host with a clock behind 1980 or past 2107 gets FAT's nearest
/// representable instant rather than a wrapped one; `FatTime` clamps and says
/// so.
fn build_time() -> FatTime {
    FatTime::from_unix_secs(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs()),
    )
}

/// Write `files` onto a formatted volume, creating the directories they name,
/// and leave the free-cluster count recorded.
///
/// Written with `toyos-fat32`, the driver the kernel appends `kernel.log` with
/// and the one its own host suite runs the volume checker against, so "the image
/// we build is clean" is a claim about the writer that is judged.
///
/// The format leaves FSInfo's free-cluster field 0xFFFFFFFF, which FAT32
/// defines as "unknown" and every host reports free space from; `free_bytes`
/// counts the FAT when the volume arrived without a hint and `sync` writes it.
fn populate(volume: &mut [u8], label: &str, files: &[(&str, &[u8])]) {
    let time = build_time();
    block_on_ready(async {
        let mut fs = Fat32::mount(Bytes::new(volume))
            .await
            .unwrap_or_else(|e| panic!("the freshly formatted {label} volume does not mount: {e}"));
        for (path, data) in files {
            if let Some((dir, _)) = path.rsplit_once('/') {
                fs.create_dir_all(dir, time).await.unwrap_or_else(|e| panic!("creating {dir}/ on {label}: {e}"));
            }
            let mut file = fs.create(path, time).await.unwrap_or_else(|e| panic!("creating {path} on {label}: {e}"));
            fs.write(&mut file, 0, data)
                .await
                .unwrap_or_else(|e| panic!("writing {} bytes to {path} on {label}: {e}", data.len()));
            fs.flush_meta(&mut file, time).await.unwrap_or_else(|e| panic!("recording {path} on {label}: {e}"));
        }
        fs.free_bytes().await.unwrap_or_else(|e| panic!("counting the {label} volume's free clusters: {e}"));
        fs.sync().await.unwrap_or_else(|e| panic!("syncing the {label} volume: {e}"));
    })
}

/// The partition firmware boots from: the bootloader, and the name of the
/// partition the kernel's log goes on. The kernel and its parameter are a
/// slot's (`create_slot_volume`), because the loader is the one part of the
/// machine that is not slotted.
fn create_esp_volume(arch: Arch, bootloader: &[u8], log_guid: Guid) -> Vec<u8> {
    let total_size = round_up_sectors(((bootloader.len() + ESP_FREE_BYTES) * 64 / 63).max(FAT32_MIN_BYTES));
    let mut volume = format_fat32(total_size, "TOYOS-BOOT");
    populate(
        &mut volume,
        "TOYOS-BOOT",
        &[
            (arch.removable_loader(), bootloader),
            // Mirrored in `loader/src/main.rs` as `\toyos\log.guid`, which
            // reads it beside itself and refuses the volume if it is not there.
            // The sixteen bytes are the GPT entry's own, in the entry's own
            // order: nothing converts them on the way to the kernel and nothing
            // converts the table's, so the comparison that decides which
            // partition holds the log cannot be got backwards.
            ("toyos/log.guid", &log_guid.0),
        ],
    );
    volume
}

/// What a slot's volume carries of one image.
struct SlotFiles<'a> {
    release: Release<'a>,
    cmdline: &'a [u8],
    signed: &'a [u8],
}

/// A slot's FAT volume of `bytes`: the kernel, its boot parameter, the two
/// boot programs and the signed header naming them and ROOT
/// (`toyos_update::slots`), or nothing for a slot no image has been installed
/// in yet.
///
/// The parameter is the actuators this boot arms, comma-separated: handed to
/// the kernel in `KernelArgs`, because the earliest actuator fires before
/// `mm::init` and there is nowhere later to fetch it from. [`params_of`] is
/// how the host asks a finished image which of them a guest booting it would
/// arm.
fn create_slot_volume(image: Option<SlotFiles<'_>>, bytes: usize) -> Vec<u8> {
    let mut volume = format_fat32(bytes, "TOYOS-SLOT");
    let files: Vec<(&str, &[u8])> = match image {
        Some(s) => vec![
            (toyos_update::slots::KERNEL_FILE, s.release.kernel),
            (toyos_update::slots::CMDLINE_FILE, s.cmdline),
            (toyos_update::slots::SUPERVISOR_FILE, s.release.supervisor),
            (toyos_update::slots::FILESERVER_FILE, s.release.fileserver),
            (toyos_update::slots::SIGNED_FILE, s.signed),
        ],
        None => Vec::new(),
    };
    populate(&mut volume, "TOYOS-SLOT", &files);
    volume
}

/// The partition the kernel's log lives on, empty until a machine boots.
///
/// Exactly [`FAT32_MIN_BYTES`], because the floor is not ours to choose and the
/// log cannot use much of it: sixteen boots at `/system/bin/logkeeper`'s `MAX_LOG_BYTES`
/// come to 16 MiB, under half of what this volume has free, and there is no
/// smaller FAT32 to cut it down to.
fn create_log_volume() -> Vec<u8> {
    let mut volume = format_fat32(FAT32_MIN_BYTES, "TOYOS-LOG");
    populate(&mut volume, "TOYOS-LOG", &[]);
    volume
}

/// Lay a table on the disk at `path`, already `len` bytes long, carrying one
/// TOYOS-DATA partition, stamp the designation at its first block, and answer
/// where it landed. The table and the stamp are all this writes, so a sparse
/// file stays sparse. The stamp names that partition's own block count, so a
/// copy of this image designates no partition of another size.
pub fn designate_data_disk(path: &Path, len: u64) -> (u64, u64) {
    let Some(data_bytes) = len
        .checked_sub(2 * PARTITION_ALIGN as u64)
        .map(|b| b / SECTOR as u64 * SECTOR as u64)
        .filter(|b| *b > 0)
    else {
        panic!("a {len}-byte disk has no room for a DATA partition");
    };
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap_or_else(|e| panic!("open {} to partition it: {e}", path.display()));
    let starts = lay(&mut file, len, &[data_row(random_guid(), data_bytes)]);
    write_at(&mut file, starts[0], &bcachefs::designation(data_bytes / SECTOR as u64), "DATA partition's designation");
    (starts[0], data_bytes)
}

/// A TOYOS-DATA partition's entry.
fn data_row(guid: Guid, len: u64) -> Row {
    Row { what: "DATA partition".into(), name: "ToyOS data".into(), kind: Guid::TOYOS_DATA, guid, len }
}

/// A version 4 GUID from the host's random source.
pub fn random_guid() -> Guid {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("the operating system's random source");
    Guid::random_v4(bytes)
}

/// A protective MBR and both copies of a GPT carrying `slots` in their
/// order, under `disk_guid`, on `disk` of `total` bytes.
pub fn write_gpt(disk: &mut (impl Write + Seek), total: u64, disk_guid: Guid, slots: &[Slot]) {
    let geometry = Geometry::new(LBA, total / u64::from(LBA)).expect("a disk that holds a table");
    let mut table = Table::empty(&geometry, disk_guid);
    assert!(slots.len() <= table.slots.len(), "{} partitions in a table of {}", slots.len(), table.slots.len());
    table.slots[..slots.len()].copy_from_slice(slots);
    let encoded = table.encode(&geometry).unwrap_or_else(|why| panic!("the partition table is not one UEFI admits: {why:?}"));
    for step in encoded.steps(true) {
        if let Step::Write { lba, bytes } = step {
            write_at(disk, lba * u64::from(LBA), bytes, "partition table");
        }
    }
}

/// Write `bytes` at byte `at` of `disk`; `what` they are is what a refusal names.
fn write_at(disk: &mut (impl Write + Seek), at: u64, bytes: &[u8], what: &str) {
    disk.seek(SeekFrom::Start(at))
        .and_then(|_| disk.write_all(bytes))
        .unwrap_or_else(|e| panic!("write the {what} at byte {at}: {e}"));
}

/// What a machine's own disk holds once the boot image `image` is installed
/// on the file at `disk`: DATA, `data_bytes` of it, and after it every
/// partition of the image under the name, type and GUID the image gave it.
///
/// **The file is one this call makes**: a path that already names anything is
/// refused, so nothing that was there is written over. DATA comes first so
/// that no image's size moves it. Only the tables, the stamp and the image's
/// blocks that are not zero are written, so the file costs the host what the
/// image does.
pub fn install(image: &[u8], disk: &Path, data_bytes: u64) {
    let mut listed = [None; 16];
    let scan = toyos_gpt::list(&mut ImageSectors(image), &mut listed)
        .unwrap_or_else(|e| panic!("the image to install carries no partition table: {e:?}"));
    assert!(scan.matched as usize <= listed.len(), "the image to install carries {} partitions", scan.matched);
    let carried: Vec<(Row, &[u8])> = listed
        .iter()
        .flatten()
        .map(|entry| {
            let part = entry.unwrap_or_else(|unplaced| panic!("the image to install states no partition: {unplaced:?}"));
            let (guid, kind) = (part.unique_guid(), part.type_guid());
            assert!(
                [Guid::EFI_SYSTEM, Guid::MICROSOFT_BASIC, Guid::TOYOS_SLOTS, Guid::TOYOS_BOOT, Guid::TOYOS_ROOT].contains(&kind),
                "partition {guid} is of type {kind}, which no boot image carries"
            );
            let name = name_of(&part);
            let at = part.first_lba() as usize * LBA as usize;
            let bytes = &image[at..at + part.lba_count().get() as usize * LBA as usize];
            (Row { what: format!("installed {name}"), name, kind, guid, len: bytes.len() as u64 }, bytes)
        })
        .collect();

    let (mut rows, volumes): (Vec<Row>, Vec<&[u8]>) = carried.into_iter().unzip();
    rows.insert(0, data_row(random_guid(), data_bytes));
    let total = disk_bytes(rows.iter().map(|row| row.len));

    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(disk)
        .unwrap_or_else(|e| panic!("{} is not installed on, being no file this install made: {e}", disk.display()));
    file.set_len(total).unwrap_or_else(|e| panic!("size {} to {total} bytes: {e}", disk.display()));

    let starts = lay(&mut file, total, &rows);
    write_at(&mut file, starts[0], &bcachefs::designation(data_bytes / SECTOR as u64), "DATA partition's designation");
    for ((row, volume), start) in rows[1..].iter().zip(volumes).zip(&starts[1..]) {
        for (i, block) in volume.chunks(PARTITION_ALIGN).enumerate().filter(|(_, block)| block.iter().any(|byte| *byte != 0)) {
            write_at(&mut file, start + (i * PARTITION_ALIGN) as u64, block, &row.what);
        }
    }
}

/// The name a table this build wrote gives `part`.
fn name_of(part: &toyos_gpt::Partition) -> String {
    String::from_utf16(part.name()).unwrap_or_else(|e| panic!("partition {}'s name is no UTF-16: {e}", part.unique_guid()))
}

/// A disk file as logical blocks, for a reader that may not hold the image.
pub struct FileSectors<'a>(pub &'a mut std::fs::File);

impl toyos_gpt::Sectors for FileSectors<'_> {
    fn lba_bytes(&self) -> u32 {
        LBA
    }

    fn lba_count(&self) -> u64 {
        self.0.metadata().map(|m| m.len()).unwrap_or(0) / u64::from(LBA)
    }

    fn read_lba(&mut self, lba: u64, buf: &mut [u8]) -> bool {
        self.0.seek(SeekFrom::Start(lba * u64::from(LBA))).is_ok() && self.0.read_exact(buf).is_ok()
    }
}

/// One partition of an image: what it is called in a refusal and in the table,
/// its type and unique GUID, its bytes, its length — at least its bytes', the
/// rest zero — and the reader that judges it before the image is published.
struct Part {
    what: &'static str,
    name: &'static str,
    kind: Guid,
    guid: Guid,
    bytes: Vec<u8>,
    len: u64,
    judge: Option<Volume>,
}

impl Part {
    /// A partition exactly as long as its bytes.
    fn full(what: &'static str, name: &'static str, kind: Guid, guid: Guid, bytes: Vec<u8>, judge: Option<Volume>) -> Self {
        let len = bytes.len() as u64;
        Self { what, name, kind, guid, bytes, len, judge }
    }
}

/// A partition's entry in a table: what it is called in a refusal and in the
/// table, its type and unique GUID, and its length.
struct Row {
    what: String,
    name: String,
    kind: Guid,
    guid: Guid,
    len: u64,
}

/// The size a disk has to be for [`lay`] to place partitions of these lengths
/// on it: an aligned gap before the first, each one aligned after the last,
/// and a gap after the last for the backup table.
fn disk_bytes(lens: impl Iterator<Item = u64>) -> u64 {
    let end = lens.fold(PARTITION_ALIGN, |end, len| align_up(end, PARTITION_ALIGN) + len as usize);
    round_up_sectors(align_up(end, PARTITION_ALIGN) + PARTITION_ALIGN) as u64
}

/// Write a protective MBR and a GPT on `disk`, `total` bytes long, placing
/// `rows` in their order, each on the first [`PARTITION_ALIGN`] boundary after
/// the last and under its own GUID: the byte each row starts at.
fn lay(disk: &mut (impl Write + Seek), total: u64, rows: &[Row]) -> Vec<u64> {
    assert_eq!(total % u64::from(LBA), 0, "image must be a whole number of {LBA}-byte sectors to be flashable");
    let mut end = 0;
    let starts: Vec<u64> = rows
        .iter()
        .map(|row| {
            // The invariant [`PARTITION_ALIGN`] exists for: the kernel mounts
            // several at once over one 4 KiB block device, and a device block
            // belonging to two volumes would be cached twice.
            assert_eq!(row.len % SECTOR as u64, 0, "the {} is {} bytes, not whole {SECTOR}-byte blocks", row.what, row.len);
            let start = align_up(end.max(1), PARTITION_ALIGN) as u64;
            end = (start + row.len) as usize;
            start
        })
        .collect();
    let lba = u64::from(LBA);
    let slots: Vec<Slot> = rows
        .iter()
        .zip(&starts)
        .map(|(row, start)| {
            Slot::new(row.kind, row.guid, start / lba, (start + row.len) / lba - 1, &row.name)
                .unwrap_or_else(|| panic!("the partition name {:?} is over {} UTF-16 units", row.name, toyos_gpt::edit::NAME_UNITS))
        })
        .collect();
    write_gpt(disk, total, random_guid(), &slots);
    starts
}

fn create_gpt_disk(parts: &[Part]) -> Vec<u8> {
    for part in parts {
        assert!(part.bytes.len() as u64 <= part.len, "the {} is {} bytes in a {}-byte partition", part.what, part.bytes.len(), part.len);
    }
    let rows: Vec<Row> = parts
        .iter()
        .map(|part| Row { what: part.what.into(), name: part.name.into(), kind: part.kind, guid: part.guid, len: part.len })
        .collect();
    let total_size = disk_bytes(rows.iter().map(|row| row.len));
    let mut disk = vec![0u8; total_size as usize];
    let starts = lay(&mut Cursor::new(&mut disk), total_size, &rows);
    for (part, start) in parts.iter().zip(&starts) {
        let start = *start as usize;
        disk[start..start + part.bytes.len()].copy_from_slice(&part.bytes);
    }

    let judged: Vec<(&str, Guid, Volume)> =
        parts.iter().filter_map(|part| part.judge.map(|kind| (part.what, part.guid, kind))).collect();
    certify(&disk, &judged).unwrap_or_else(|refusal| panic!("{refusal}"));
    disk
}

/// A disk image as logical blocks, so a GPT parser can read it without a file.
pub(crate) struct ImageSectors<'a>(pub(crate) &'a [u8]);

impl toyos_gpt::Sectors for ImageSectors<'_> {
    fn lba_bytes(&self) -> u32 {
        LBA
    }

    fn lba_count(&self) -> u64 {
        self.0.len() as u64 / u64::from(LBA)
    }

    fn read_lba(&mut self, lba: u64, buf: &mut [u8]) -> bool {
        let at = lba as usize * LBA as usize;
        let Some(block) = self.0.get(at..at + buf.len()) else { return false };
        buf.copy_from_slice(block);
        true
    }
}

/// What a partition's bytes are, so [`certify`] knows which judge to put them
/// in front of.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Volume {
    Fat32,
    Root,
}

/// Why the image at `disk` may not be published, or `Ok` because its checks
/// find it sound.
///
/// `toyos-gpt` finds each partition by the unique GUID the table claims for it,
/// then `toyos-fat32-check` judges a FAT volume against fatgen103 and
/// bcachefs's reader and fsck judge ROOT. The fsck shares the writer's
/// specification, so it catches the writer contradicting itself; upstream's
/// fsck is ROOT's independent judge, outside the build. A volume the table
/// misplaces fails its format check on whatever it does land on, which is why
/// the extents are not compared separately.
fn certify(disk: &[u8], parts: &[(&str, toyos_gpt::Guid, Volume)]) -> Result<(), String> {
    for (what, guid, kind) in parts {
        let located = toyos_gpt::locate(&mut ImageSectors(disk), *guid)
            .map_err(|e| format!("toyos-gpt cannot find the {what} ({guid}) on this image: {e:?}"))?;
        let at = located.partition().first_lba() as usize * LBA as usize;
        let bytes = located.partition().lba_count().get() as usize * LBA as usize;
        let volume = disk
            .get(at..at + bytes)
            .ok_or_else(|| format!("the {what} runs to byte {} of a {}-byte image", at + bytes, disk.len()))?;
        match kind {
            Volume::Fat32 => {
                let complaints = toyos_fat32_check::check(volume);
                if !complaints.is_empty() {
                    return Err(format!(
                        "toyos-fat32-check refuses the {what} of the image this build wrote:\n{}",
                        toyos_fat32_check::describe(&complaints)
                    ));
                }
            }
            Volume::Root => {
                let refused = |why: String| format!("bcachefs refuses the {what} of the image this build wrote: {why}");
                let root = mount_root(volume).map_err(refused)?;
                let found = block_on_ready(engine::fsck(&root)).map_err(|e| refused(format!("{e:?}")))?;
                if !found.is_empty() {
                    return Err(refused(format!("{found:?}")));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bcachefs::engine::DirentType;

    /// A ROOT image with one file in it, for the tests that need a real one
    /// rather than a placeholder: the assembler reads its superblock.
    fn tiny_root() -> Vec<u8> {
        create_root_image(&[("etc/os-release".to_string(), b"release".to_vec())], &[], true)
    }

    /// Placeholder programs: what these tests judge is where each goes.
    fn release<'a>(kernel: &'a [u8], root: &'a [u8]) -> Release<'a> {
        Release { kernel, supervisor: b"\x7fELF supervisor", fileserver: b"\x7fELF fileserver", root }
    }

    /// What a slot's volume carries of `release`.
    fn slot_files<'a>(release: Release<'a>, s: &'a Signed) -> SlotFiles<'a> {
        SlotFiles { release, cmdline: &s.cmdline, signed: &s.signed }
    }

    /// A fixed key: what these tests judge is the image, not whose it is.
    fn key() -> crate::signing::Key {
        crate::signing::Key::throwaway_from([0x42; 32])
    }

    fn signing(key: &crate::signing::Key) -> Signing<'_> {
        Signing { key, version: 7 }
    }

    /// Every name on the ROOT volume `bytes`, files and symlinks with their
    /// sizes, directories with 0, as the reader lists them.
    fn root_names(bytes: &[u8]) -> Vec<(String, DirentType, u64)> {
        let root = mount_root(bytes).expect("ROOT mounts");
        let mut out: Vec<(String, DirentType, u64)> = block_on_ready(async {
            let mut out = Vec::new();
            for (path, entry) in root.names().await.expect("ROOT lists") {
                let size = if entry.kind == DirentType::Directory { 0 } else { root.inode(entry.ino).await.expect("its inode").size };
                out.push((String::from_utf8(path).expect("a name the build gave"), entry.kind, size));
            }
            out
        });
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// One file set built twice is one byte string: nothing the build
    /// writes on ROOT comes from the clock or from a random source.
    #[test]
    fn one_set_built_twice_is_one_image() {
        let files: Vec<(String, Vec<u8>)> =
            (0..40u32).map(|i| (format!("bin/prog{i:02}"), vec![(i % 251) as u8; 20_000 + i as usize * 17])).collect();
        let symlinks = vec![("bin/ls".to_string(), "/system/bin/prog00".to_string())];
        let first = create_root_image(&files, &symlinks, true);
        let second = create_root_image(&files, &symlinks, true);
        assert!(first == second, "one file set built twice wrote two {}-byte images", first.len());
    }

    /// Bucket sizes double as a device crosses 64 MiB, so a ROOT around it
    /// is laid out in 16 KiB buckets on one side and 32 KiB ones on the
    /// other. Every file set across that edge fits the size the build
    /// gives it, checks clean and reads back whole.
    #[test]
    fn file_sets_across_a_bucket_size_edge_fit_and_read_back() {
        let mut sides = std::collections::BTreeSet::new();
        for kib in (58880..59904).step_by(256) {
            let big: Vec<u8> = (0..kib << 10).map(|i| (i / 4096 + kib) as u8).collect();
            let files: Vec<(String, Vec<u8>)> = std::iter::once(("bin/big".to_string(), big))
                .chain((0..200u32).map(|i| (format!("share/s{i:03}"), vec![i as u8; 3000 + i as usize])))
                .collect();
            let image = create_root_image(&files, &[], true);
            sides.insert(image.len() > 64 << 20);
            let root = mount_root(&image).expect("it mounts");
            assert_eq!(block_on_ready(engine::fsck(&root)), Ok(Vec::new()), "{kib} KiB");
            for (name, data) in &files {
                assert!(root_file(&image, name).expect("it reads") == *data, "{kib} KiB: {name}");
            }
        }
        assert_eq!(sides.len(), 2, "the file sets do not straddle the edge");
    }

    /// The volumes this build writes break no rule of the format, and the
    /// gate is silence rather than sameness: a suite that could only ask the
    /// guest to add no *new* complaint hides every complaint the writer left.
    /// Here rather than in the boot suite because it needs no guest, no QEMU and
    /// no kernel — a claim about the writer, failing in seconds.
    #[test]
    fn the_volumes_this_build_writes_break_no_format_rule() {
        let key = key();
        let root = tiny_root();
        let s = signed(release(b"kernel", &root), "", signing(&key));
        for (what, volume) in [
            ("ESP", create_esp_volume(Arch::X86_64, b"bootloader", random_guid())),
            ("slot volume", create_slot_volume(Some(slot_files(release(b"kernel", &root), &s)), FAT32_MIN_BYTES)),
            ("empty slot volume", create_slot_volume(None, FAT32_MIN_BYTES)),
            ("log volume", create_log_volume()),
        ] {
            let complaints = toyos_fat32_check::check(&volume);
            assert!(
                complaints.is_empty(),
                "the {what} this build writes is not a clean FAT32 volume:\n{}",
                toyos_fat32_check::describe(&complaints)
            );
        }
    }

    /// The one partition of `kind` on `disk`.
    fn only(disk: &[u8], kind: toyos_gpt::Guid) -> toyos_gpt::Guid {
        only_partition(&mut ImageSectors(disk), kind).expect("the image carries one").unique_guid()
    }

    /// Publishing a flash target runs every reader over the assembled image, and
    /// a damaged one is refused by name: each mutation is staged on an image
    /// [`create_gpt_disk`] just certified, so each refusal is its own.
    #[test]
    fn a_damaged_image_is_refused_by_the_reader_that_caught_it() {
        let root_image = tiny_root();
        let key = key();
        let disk = create_boot_image(Arch::X86_64, b"bootloader", release(b"kernel", &root_image), "", signing(&key), None);
        let log = only(&disk, toyos_gpt::Guid::MICROSOFT_BASIC);
        let root = root_partition_guid_of(&disk);
        let parts =
            [("log partition", log, Volume::Fat32), ("root partition", root, Volume::Root)];
        certify(&disk, &parts).expect("the image this build writes certifies");

        let mut torn = disk.clone();
        torn[510] ^= 0xff;
        let refusal = certify(&torn, &parts).expect_err("a torn protective MBR is not a GPT");
        assert!(refusal.contains("toyos-gpt"), "{refusal}");
        assert!(refusal.contains("NoProtectiveMbr"), "{refusal}");

        let start_of = |guid| {
            toyos_gpt::locate(&mut ImageSectors(&disk), guid)
                .expect("the partition is on the image")
                .partition()
                .first_lba() as usize
                * LBA as usize
        };

        let mut broken = disk.clone();
        broken[start_of(log) + 510] ^= 0xff;
        let refusal = certify(&broken, &parts).expect_err("that volume is not FAT32");
        assert!(refusal.contains("toyos-fat32-check"), "{refusal}");

        // A ROOT of zeros: no superblock copy at all.
        let root_at = start_of(root);
        let mut broken = disk.clone();
        broken[root_at..root_at + root_image.len()].fill(0);
        let refusal = certify(&broken, &parts).expect_err("that volume is not bcachefs");
        assert!(refusal.contains("bcachefs refuses"), "{refusal}");
    }

    /// The unique GUID the table drew for the one TOYOS-ROOT-typed partition.
    fn root_partition_guid_of(disk: &[u8]) -> toyos_gpt::Guid {
        only(disk, toyos_gpt::Guid::TOYOS_ROOT)
    }

    /// Every file on `volume`, sorted.
    fn files_of(volume: Vec<u8>) -> Vec<String> {
        let walked = block_on_ready(async {
            let mut fs = Fat32::mount(Bytes::new(volume)).await.expect("mount a volume we just built");
            fs.walk("", 64).await
        });
        let mut found: Vec<String> = walked
            .expect("walk the volume")
            .into_iter()
            .map(|(path, _)| path.trim_start_matches('/').to_string())
            .filter(|path| !path.ends_with('/'))
            .collect();
        found.sort();
        found
    }

    /// And it is clean because it is right, not because it is empty: a
    /// `populate` that wrote nothing at all would satisfy the gate above.
    /// Exactly these and nothing else, because an unnamed file on a volume is
    /// one the volume pays for and nothing loads.
    #[test]
    fn the_esp_and_a_slot_carry_what_the_bootloader_looks_for() {
        assert_eq!(
            files_of(create_esp_volume(Arch::X86_64, b"bootloader", random_guid())),
            ["EFI/BOOT/BOOTx64.EFI", "toyos/log.guid"]
        );
        let key = key();
        let root = tiny_root();
        let s = signed(release(b"kernel", &root), "", signing(&key));
        assert_eq!(
            files_of(create_slot_volume(Some(slot_files(release(b"kernel", &root), &s)), FAT32_MIN_BYTES)),
            [
                toyos_update::slots::CMDLINE_FILE,
                toyos_update::slots::FILESERVER_FILE,
                toyos_update::slots::SIGNED_FILE,
                toyos_update::slots::KERNEL_FILE,
                toyos_update::slots::SUPERVISOR_FILE,
            ]
        );
        assert!(files_of(create_slot_volume(None, FAT32_MIN_BYTES)).is_empty());
    }

    /// **The host's own reading of what the loader checks**: the marked slot's
    /// signed header verifies under the key the image was signed with and names
    /// the kernel, the boot parameter, the two programs and ROOT the image
    /// carries, byte for byte, and ROOT carries neither program; the table names
    /// the slot's partitions; and an update image for the same parts splits into
    /// the same header and sections. A second slot is present, empty, and not
    /// marked.
    #[test]
    fn an_image_s_marked_slot_is_signed_over_exactly_what_it_carries() {
        let key = key();
        let root = tiny_root();
        let dir = toyos_tmpdir::TempDir::new("image-slot");
        let path = dir.join("slotted.img");
        let disk = create_boot_image(Arch::X86_64, b"bootloader", release(b"\x7fELF kernel", &root), "sched-fast-health", signing(&key), Some(SecondSlot { root_bytes: 8 << 20 }));
        std::fs::write(&path, &disk).expect("write the image");
        let mut file = std::fs::File::open(&path).expect("open the image");
        let table = slot_table_of(&mut file).expect("the slot table");
        assert_eq!(table.marked, toyos_update::slots::Which::A);
        let a = table.slots[0].expect("slot A");
        let b = table.slots[1].expect("slot B");
        assert_eq!(a.version, 7);
        assert!(read_file_on(&mut file, b.boot, toyos_update::slots::SIGNED_FILE).is_err(), "an empty slot is unsigned");
        assert_eq!(partition_extent(&mut file, b.root).expect("slot B's ROOT").1, 8 << 20);

        let signed = read_file_on(&mut file, a.boot, toyos_update::slots::SIGNED_FILE).expect("the signed header");
        let signed: [u8; toyos_update::image::SIGNED_BYTES] = signed.try_into().expect("a signed header's length");
        let header = toyos_update::image::Header::parse(&signed).expect("a header");
        let bytes: [u8; toyos_update::image::HEADER_BYTES] = signed[..toyos_update::image::HEADER_BYTES].try_into().unwrap();
        toyos_update::sig::verify(&key.public(), &bytes, &toyos_update::image::signature_of(&signed)).expect("the signature");
        let kernel = read_file_on(&mut file, a.boot, toyos_update::slots::KERNEL_FILE).expect("the kernel");
        let cmdline = read_file_on(&mut file, a.boot, toyos_update::slots::CMDLINE_FILE).expect("the cmdline");
        let supervisor = read_file_on(&mut file, a.boot, toyos_update::slots::SUPERVISOR_FILE).expect("the supervisor");
        let fileserver = read_file_on(&mut file, a.boot, toyos_update::slots::FILESERVER_FILE).expect("the file server");
        let (at, _) = partition_extent(&mut file, a.root).expect("slot A's ROOT");
        let on_disk = &disk[at as usize..at as usize + header.root().len as usize];
        assert_eq!(kernel, b"\x7fELF kernel");
        assert_eq!(cmdline, b"sched-fast-health", "the boot parameter is the image's parameters and names no filesystem");
        let placeholder = release(b"", b"");
        assert_eq!((&supervisor[..], &fileserver[..]), (placeholder.supervisor, placeholder.fileserver));
        assert_eq!(on_disk, &root[..]);
        let carried = toyos_update::image::Sections { kernel: &kernel, cmdline: &cmdline, supervisor: &supervisor, fileserver: &fileserver, root: on_disk };
        assert_eq!(header, toyos_update::image::Header::of(7, &carried));
        drop(file);
        drop(dir);

        let update = update_image(release(b"\x7fELF kernel", &root), "sched-fast-health", signing(&key));
        let parts = toyos_update::image::Parts::split(&update).expect("an update image splits");
        assert_eq!(parts.header, header);
        assert_eq!(parts.sections, carried);
        assert_eq!(parts.signed, &signed);
    }

    /// Every partition of `image` is on `disk` under its GUID, its name and with
    /// its bytes, found by the reader the kernel and `diskserver` use: the
    /// names, in the image's order.
    fn carries_every_partition_of(disk: &mut std::fs::File, image: &[u8]) -> Vec<String> {
        let mut names = Vec::new();
        let mut listed = [None; 16];
        toyos_gpt::list(&mut ImageSectors(image), &mut listed).expect("the image's table");
        let parts: Vec<toyos_gpt::Partition> = listed.iter().flatten().map(|entry| entry.expect("a placed entry")).collect();
        assert!(parts.len() >= 5, "an image of {} partitions", parts.len());
        for part in parts {
            let at = part.first_lba() as usize * LBA as usize;
            let len = part.lba_count().get() as usize * LBA as usize;
            let on_disk = toyos_gpt::locate(&mut FileSectors(disk), part.unique_guid()).expect("the partition on the disk").partition();
            assert_eq!(on_disk.type_guid(), part.type_guid(), "{}", part.unique_guid());
            assert_eq!(on_disk.lba_count(), part.lba_count(), "{}", part.unique_guid());
            let mut installed = vec![0u8; len];
            disk.seek(SeekFrom::Start(on_disk.first_lba() * u64::from(LBA)))
                .and_then(|_| disk.read_exact(&mut installed))
                .expect("read it back");
            assert!(installed == image[at..at + len], "{} was installed as other bytes", part.unique_guid());
            assert_eq!(name_of(&on_disk), name_of(&part), "{}", part.unique_guid());
            names.push(name_of(&part));
        }
        names
    }

    /// An install puts a designated DATA first and the image after it, on a
    /// file it makes; a path that names a file already is refused, and that
    /// file is left as it was.
    #[test]
    fn an_install_makes_its_disk_and_refuses_one_that_exists() {
        const DATA: u64 = 4 << 20;
        let key = key();
        let image = create_boot_image(Arch::X86_64, b"bootloader", release(b"kernel", &tiny_root()), "", signing(&key), None);
        let dir = toyos_tmpdir::TempDir::new("image-install");
        let path = dir.join("disk.img");

        install(&image, &path, DATA);
        let mut disk = std::fs::File::open(&path).expect("open the disk");
        assert_eq!(
            carries_every_partition_of(&mut disk, &image),
            ["EFI System", "ToyOS slots", "ToyOS log", "ToyOS slot A", "ToyOS root A"]
        );
        let data = only_partition(&mut FileSectors(&mut disk), toyos_gpt::Guid::TOYOS_DATA).expect("one DATA partition");
        assert_eq!(
            (data.first_lba() * u64::from(LBA), data.lba_count().get() * u64::from(LBA)),
            (PARTITION_ALIGN as u64, DATA)
        );
        let mut stamp = [0u8; SECTOR];
        disk.seek(SeekFrom::Start(PARTITION_ALIGN as u64)).and_then(|_| disk.read_exact(&mut stamp)).expect("read DATA");
        assert_eq!(stamp, bcachefs::designation(DATA / SECTOR as u64));
        drop(disk);

        let theirs = dir.join("theirs.img");
        std::fs::write(&theirs, b"somebody's").expect("write a file");
        let refused = std::panic::catch_unwind(|| install(&image, &theirs, DATA)).expect_err("an install on a file that exists");
        let said = refused.downcast_ref::<String>().expect("a refusal in words");
        assert!(said.contains("theirs.img"), "the refusal names no path: {said}");
        assert_eq!(std::fs::read(&theirs).expect("read it back"), b"somebody's");
    }

    /// **One ordering of one set is one image.** The same files handed over
    /// backwards have to come out as one UUID and one byte string, or two
    /// builds of one tree are two images, and the name the file server says
    /// ROOT by names a filesystem the next build has not got.
    #[test]
    fn one_ordering_of_one_set_is_one_image() {
        let files: Vec<(String, Vec<u8>)> = vec![
            ("bin/shell".to_string(), b"shell-binary".to_vec()),
            ("bin/toybox".to_string(), (0..40_000u32).map(|i| (i ^ 0x5A) as u8).collect()),
            ("etc/system.manifest".to_string(), b"[start]\nsupervisor\n".to_vec()),
            ("share/empty".to_string(), Vec::new()),
        ];
        let symlinks = vec![
            ("bin/ls".to_string(), "/system/bin/toybox".to_string()),
            ("bin/cat".to_string(), "/system/bin/toybox".to_string()),
            ("bin/echo".to_string(), "/system/bin/toybox".to_string()),
        ];

        let forwards = create_root_image(&files, &symlinks, true);

        let mut backwards_files = files.clone();
        backwards_files.reverse();
        let mut backwards_symlinks = symlinks.clone();
        backwards_symlinks.reverse();
        let backwards = create_root_image(&backwards_files, &backwards_symlinks, true);

        assert_eq!(
            root_uuid_of(&forwards).unwrap(),
            root_uuid_of(&backwards).unwrap(),
            "one set of files named two filesystems"
        );
        assert_eq!(forwards.len(), backwards.len());
        assert!(
            forwards == backwards,
            "one set of files wrote two different {}-byte volumes under one name {}",
            forwards.len(),
            root_uuid_of(&forwards).unwrap()
        );

        // And it is not a constant: a different set is a different name.
        let mut other = files;
        other[0].1.push(b'!');
        assert_ne!(
            root_uuid_of(&forwards).unwrap(),
            root_uuid_of(&create_root_image(&other, &symlinks, true)).unwrap(),
            "one byte of one file changed and the filesystem kept its name"
        );
    }

    /// **ROOT reads back as it was given**, asked of the finished image:
    /// `toyos-gpt` finds the partition by type where the table placed it, and
    /// bcachefs's reader lists what its mkfs put there.
    ///
    /// Every name, size, content hash and symlink target against the list
    /// handed to [`create_root_image`]: a listing that merely parsed would
    /// pass a far weaker claim.
    #[test]
    fn the_root_partition_reads_back_as_the_files_the_build_put_in_it() {
        let files: Vec<(String, Vec<u8>)> = vec![
            ("bin/shell".to_string(), b"shell-binary".to_vec()),
            // Multi-block, so an extent list that stopped after the first is
            // caught by the hash rather than by the size.
            ("bin/toybox".to_string(), (0..40_000u32).map(|i| (i ^ 0x5A) as u8).collect()),
            ("etc/system.manifest".to_string(), b"[start]\nsupervisor\n".to_vec()),
            ("share/empty".to_string(), Vec::new()),
        ];
        let symlinks = vec![("bin/ls".to_string(), "/system/bin/toybox".to_string())];

        let root_image = create_root_image(&files, &symlinks, true);
        let key = key();
        let disk = create_boot_image(Arch::X86_64, b"bootloader", release(b"kernel", &root_image), "", signing(&key), None);

        // Located by *type*, through the parser the kernel uses, at the offset
        // the table gives — never at the one the writer computed.
        let part = only_partition(&mut ImageSectors(&disk), toyos_gpt::Guid::TOYOS_ROOT)
            .expect("a boot image carries exactly one TOYOS-ROOT partition");
        let at = part.first_lba() as usize * LBA as usize;
        let bytes = part.lba_count().get() as usize * LBA as usize;
        let volume = disk[at..at + bytes].to_vec();

        let listed = root_names(&volume);
        let mut want: Vec<(String, DirentType, u64)> = ["bin", "etc", "share"].iter().map(|d| (d.to_string(), DirentType::Directory, 0)).collect();
        want.extend(files.iter().map(|(name, data)| (name.clone(), DirentType::Regular, data.len() as u64)));
        // A symlink's size counts the NUL stored after its target.
        want.extend(symlinks.iter().map(|(name, to)| (name.clone(), DirentType::Symlink, to.len() as u64 + 1)));
        want.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(listed, want, "ROOT does not hold the names, kinds and sizes it was built from");

        let root = mount_root(&volume).expect("the ROOT partition mounts");
        for (name, data) in &files {
            let read = root_file(&volume, name).unwrap_or_else(|e| panic!("read {name}: {e}"));
            assert_eq!(
                crate::sha256(&read),
                crate::sha256(data),
                "{name} reads back as {} bytes that are not the {} it was given",
                read.len(),
                data.len()
            );
        }
        for (name, target) in &symlinks {
            let read = block_on_ready(async { root.readlink(root.resolve(name.as_bytes()).await?).await })
                .unwrap_or_else(|e| panic!("readlink {name}: {e:?}"));
            assert_eq!(read, target.as_bytes());
        }
    }

    /// **The independent oracle for the hierarchy.** ROOT is served at
    /// `/system`, so every absolute path the shipped image declares is
    /// `/system/` plus a name ROOT carries; the file server's row names no
    /// path, its image being the slot's. Asked of a
    /// finished image: `toyos-gpt` finds ROOT by type where the table says,
    /// bcachefs's reader opens it, and `toyos-manifest` parses the records out
    /// of the volume.
    #[test]
    fn every_declared_path_resolves_inside_the_root_partition() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let (manifest, symlinks) = crate::build::manifest_and_symlinks(&root.join("images/system.toml"));
        let declared = toyos_manifest::parse(
            std::str::from_utf8(&manifest).expect("the manifest this build renders is text"),
        );
        let on_root: Vec<&str> = declared.programs.iter().filter_map(|p| p.source.path()).collect();
        assert_eq!(
            declared.programs.len() - on_root.len(),
            1,
            "the shipped image declares one program run from a boot image, the file server"
        );

        // Placeholders: what this judges is the path scheme, and the bytes of a
        // real binary say nothing about it.
        let mut files: Vec<(String, Vec<u8>)> =
            on_root.iter().map(|path| (under_system(path).to_string(), b"placeholder".to_vec())).collect();
        files.push((toyos_manifest::PATH.to_string(), manifest.clone()));

        let root_image = create_root_image(&files, &symlinks, true);
        let disk = create_boot_image(Arch::X86_64, b"bootloader", release(b"kernel", &root_image), "", signing(&key()), None);

        let part = only_partition(&mut ImageSectors(&disk), toyos_gpt::Guid::TOYOS_ROOT)
            .expect("a boot image carries exactly one TOYOS-ROOT partition");
        let at = part.first_lba() as usize * LBA as usize;
        let bytes = part.lba_count().get() as usize * LBA as usize;
        let volume = &disk[at..at + bytes];

        // As the volume holds it, not as the renderer returned it.
        let on_volume = root_file(volume, toyos_manifest::PATH)
            .expect("ROOT carries the manifest where the guest path names it");
        assert_eq!(on_volume, manifest, "the manifest on ROOT is not the one the build rendered");
        assert_eq!(
            toyos_manifest::GUEST_PATH,
            format!("/system/{}", toyos_manifest::PATH),
            "the path a process opens is not where ROOT carries the manifest"
        );

        let names: std::collections::BTreeSet<String> = root_names(volume).into_iter().map(|(name, _, _)| name).collect();
        for &path in &on_root {
            let name = under_system(path);
            assert_ne!(name, path, "{path} is not under the mount ROOT gets");
            assert!(names.contains(name), "ROOT has no {name} for the declared path {path}");
        }
    }

    /// A declared absolute path as ROOT carries it; unchanged off that mount.
    fn under_system(path: &str) -> &str {
        path.strip_prefix("/system/").unwrap_or(path)
    }
}
