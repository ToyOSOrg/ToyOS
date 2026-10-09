use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::num::NonZeroU64;
use std::path::Path;

use bcachefs::{BlockBuf, BlockIO, BlockNum, Formatted, FsUuid, Superblock, VecBlockIO};

use crate::arch::Arch;
use sha2::{Digest, Sha256};
use toyos_fat32::{BlockAccess, Fat32, FatTime, IoError};
use toyos_gpt::Guid;

/// The image that goes on the ROOT partition, named by a UUID **derived, never
/// drawn**: two builds of one tree have to agree on the name the kernel
/// argument carries.
///
/// **A set, not a sequence.** Both lists are sorted by name here, so the
/// volume's bytes and the UUID over them are a function of what the caller
/// holds and not of the order it happened to hand it over in — a caller that
/// walked a hash map, or a directory, would otherwise make one tree two images.
/// `one_ordering_of_one_set_is_one_image` is the arm.
pub fn create_root_image(
    files: &[(String, Vec<u8>)],
    symlinks: &[(String, String)],
    quiet: bool,
) -> Vec<u8> {
    let mut files: Vec<&(String, Vec<u8>)> = files.iter().collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut symlinks: Vec<&(String, String)> = symlinks.iter().collect();
    symlinks.sort_by(|a, b| a.0.cmp(&b.0));

    let data_size: usize = files.iter().map(|(_, d)| d.len()).sum::<usize>();
    let total_entries = files.len() + symlinks.len();
    // Estimate: superblock(1) + bitmap + btree nodes + data blocks + backup(1) + 10% padding
    let data_blocks = data_size.div_ceil(4096);
    let btree_blocks = (total_entries / 30).max(2);
    let overhead = 64;
    let total_blocks = (1 + overhead + btree_blocks + data_blocks) * 11 / 10;
    let estimate = align_up(total_blocks.max(64), PARTITION_ALIGN / 4096) as u64;
    let spacious = format_root(&files, &symlinks, estimate, quiet);
    trim_root(spacious)
}

/// `spacious`, cut to the blocks its own build claimed and no more: ROOT is
/// read-only, and the loader reads and the kernel keeps every block of it, so
/// a free block on it is a read and a page for nothing.
///
/// Trimmed, never rebuilt at the smaller count: mkfs draws its hash seed from
/// `block_count` (`Formatted::format`'s "deterministic for reproducible
/// builds"), so a second mkfs at a different device size keys every name
/// differently and can shape a bigger btree than the first build's headroom
/// ever measured — the reformat this replaced ran out by one block on
/// exactly that, on a file set nothing about its *bytes* explains. Cutting
/// the same bytes down needs nothing the first build did not already prove:
/// mkfs never frees a block once claimed, so every allocation sits below
/// `next_alloc` with no hole beneath it, and everything from there to the
/// alignment boundary is free to discard but for the one block the trimmed
/// image's own backup superblock claims.
fn trim_root(spacious: Vec<u8>) -> Vec<u8> {
    let sb = superblock_of(&spacious);
    assert!(sb.next_alloc > 0, "an mkfs cursor at zero claims nothing was allocated");
    let kept = (align_up((sb.next_alloc + 1) as usize, PARTITION_ALIGN / 4096) as u64).min(sb.block_count);

    let mut bytes = spacious;
    bytes.truncate((kept * 4096) as usize);
    let io = VecBlockIO::from_vec(bytes);

    // The kept bitmap already marks every block below `next_alloc` used and
    // was sized for a device at least this large (`Superblock::check` allows
    // a bitmap bigger than a device needs, never smaller), so the one bit
    // this trim must still set is the backup superblock's new block: free in
    // that bitmap, since it sits at or past every allocation mkfs made.
    let backup = BlockNum::new(kept - 1);
    let (bitmap_block, byte_off, bit) = bitmap_location(&sb, backup);
    let mut buf = BlockBuf::zeroed();
    io.read_block(bitmap_block, &mut buf).expect("read a bitmap block this trim kept");
    buf.as_bytes_mut()[byte_off] |= 1 << bit;
    io.write_block(bitmap_block, &buf).expect("write a bitmap block this trim kept");

    let mut trimmed = sb;
    trimmed.block_count = kept;
    trimmed.free_blocks = kept - trimmed.next_alloc - 1;
    trimmed.write(&io).expect("write the trimmed superblock");
    io.into_vec()
}

/// Where `block`'s bit lives in `sb`'s bitmap: the bitmap block, the byte
/// within it, and the bit within that byte. `BitmapAllocator::bit_of` computes
/// the same triple from the allocator's own fields; this is the version a
/// caller outside the crate can reach, over an image it did not format.
fn bitmap_location(sb: &Superblock, block: BlockNum) -> (BlockNum, usize, u8) {
    let byte_idx = block.raw() / 8;
    (
        BlockNum::new(sb.bitmap_start.raw() + byte_idx / 4096),
        (byte_idx % 4096) as usize,
        (block.raw() % 8) as u8,
    )
}

/// A ROOT volume of `total_blocks`, holding `files` and `symlinks` in the
/// order given. Whole alignment units: `Superblock::check` refuses a
/// superblock whose block count is not its view's exactly, so a partitioner
/// rounding the size up to the alignment would leave an image nothing can
/// mount.
fn format_root(
    files: &[&(String, Vec<u8>)],
    symlinks: &[&(String, String)],
    total_blocks: u64,
    quiet: bool,
) -> Vec<u8> {
    let io = VecBlockIO::new(total_blocks);
    let mut fs = Formatted::format(io).expect("format an in-memory image");

    for (name, data) in files {
        if !quiet {
            eprintln!("root: adding '{}' ({} bytes)", name, data.len());
        }
        fs.create(name, data, 0)
            .unwrap_or_else(|e| panic!("root: failed to add '{}': {:?}", name, e));
    }

    for (name, target) in symlinks {
        if !quiet {
            eprintln!("root: symlink '{}' -> '{}'", name, target);
        }
        fs.create_symlink(name, target, 0)
            .unwrap_or_else(|e| panic!("root: failed to symlink '{}' -> '{}': {:?}", name, target, e));
    }

    fs.set_uuid(root_uuid(files, symlinks));
    fs.into_io().expect("write an in-memory image").into_vec()
}

/// A name for exactly this set of files and symlinks, in the order
/// [`create_root_image`] sorted them into. Lengths go into the digest beside
/// the bytes, so no two entries can run together into an input a different
/// split would also produce.
fn root_uuid(files: &[&(String, Vec<u8>)], symlinks: &[&(String, String)]) -> FsUuid {
    let mut hasher = Sha256::new();
    let mut field = |bytes: &[u8]| {
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    };
    for (name, data) in files {
        field(name.as_bytes());
        field(data);
    }
    for (name, target) in symlinks {
        field(name.as_bytes());
        field(target.as_bytes());
    }
    let digest = hasher.finalize();
    let mut uuid = [0u8; 16];
    uuid.copy_from_slice(&digest[..16]);
    FsUuid(uuid)
}

/// The name the ROOT image `bytes` carries, read back out of its superblock, so
/// the kernel argument says what the image says rather than what whoever
/// assembled it meant to stamp.
pub fn root_uuid_of(bytes: &[u8]) -> FsUuid {
    superblock_of(bytes).uuid
}

fn superblock_of(bytes: &[u8]) -> Superblock {
    let block = <[u8; 4096]>::try_from(&bytes[..4096]).expect("a bcachefs image is at least one block");
    Superblock::parse(&BlockBuf(block)).expect("the ROOT image this build wrote carries a superblock")
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

/// The sections of one image, as a slot carries them and an update sends them.
struct Sections {
    kernel: Vec<u8>,
    cmdline: Vec<u8>,
    signed: [u8; toyos_update::image::SIGNED_BYTES],
}

fn sections(kernel: &[u8], root: &[u8], params: &str, signing: Signing<'_>) -> Sections {
    let cmdline = cmdline_with_root(root_uuid_of(root), params).into_bytes();
    let header = toyos_update::image::Header::of(signing.version, kernel, &cmdline, root);
    Sections { kernel: kernel.to_vec(), signed: signing.key.sign(&header), cmdline }
}

/// The image `ssh <machine> update` takes on its standard input: the signed
/// header, then the kernel, the boot parameter and ROOT, in the order the
/// header names them (`toyos_update::image`).
pub fn update_image(kernel: &[u8], root: &[u8], params: &str, signing: Signing<'_>) -> Vec<u8> {
    let s = sections(kernel, root, params, signing);
    let mut out = s.signed.to_vec();
    out.extend_from_slice(&s.kernel);
    out.extend_from_slice(&s.cmdline);
    out.extend_from_slice(root);
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
    kernel_bytes: &[u8],
    bl_bytes: &[u8],
    root_bytes: &[u8],
    params: &str,
    signing: Signing<'_>,
    second: Option<SecondSlot>,
) -> Vec<u8> {
    // Drawn here and written twice: into the GPT entry that *is* the log
    // partition, and into a file on the ESP that the bootloader hands the
    // kernel. The kernel is given the partition by name; nothing anywhere goes
    // looking for one by type or by format. Every slot partition is named the
    // same way, by the slot table.
    let (log_guid, esp_guid, table_guid) = (crate::gptwrite::random_guid(), crate::gptwrite::random_guid(), crate::gptwrite::random_guid());
    let a = (crate::gptwrite::random_guid(), crate::gptwrite::random_guid());
    let b = second.map(|room| (crate::gptwrite::random_guid(), crate::gptwrite::random_guid(), room));

    let s = sections(kernel_bytes, root_bytes, params, signing);
    // Room for a kernel twice this one's size beside it, so an update can
    // write a new kernel into a slot's volume before the old one is gone.
    let slot_bytes = round_up_sectors(((kernel_bytes.len() * 2 + ESP_FREE_BYTES) * 64 / 63).max(FAT32_MIN_BYTES));
    let a_boot = create_slot_volume(Some(&s), slot_bytes);
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
        Part::full("slot A's ROOT", "ToyOS root A", Guid::TOYOS_ROOT, a.1, root_bytes.to_vec(), Some(Volume::Root)),
    ];
    if let Some((boot, root, room)) = b {
        parts.push(Part::full("slot B's volume", "ToyOS slot B", Guid::TOYOS_BOOT, boot, create_slot_volume(None, slot_bytes), Some(Volume::Fat32)));
        let len = align_up(room.root_bytes as usize, PARTITION_ALIGN) as u64;
        parts.push(Part { what: "slot B's ROOT", name: "ToyOS root B", kind: Guid::TOYOS_ROOT, guid: root, bytes: Vec::new(), len, judge: None });
    }
    create_gpt_disk(&parts)
}

/// The boot parameter as a slot carries it: ROOT's name, then `params`.
fn cmdline_with_root(root: FsUuid, params: &str) -> String {
    if params.is_empty() {
        format!("root={root}")
    } else {
        format!("root={root},{params}")
    }
}

/// The actuator list an image is armed with, read back off the image.
///
/// `root=` is not an actuator and is on every image: this answers what a boot
/// would *arm*, which is the other reading of the same string
/// (`toyos_abi::boot::actuators`).
pub fn params_of(path: &Path) -> Result<Vec<String>, String> {
    let text = cmdline_of(path)?;
    Ok(toyos_abi::boot::actuators(&text).map(str::to_string).collect())
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
/// its name, and the file `name` it carries, read with the driver the kernel
/// mounts it with.
pub fn root_file_on(path: &Path, name: &str) -> Result<(FsUuid, Vec<u8>), String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("opening {}: {e}", path.display()))?;
    let table = slot_table_of(&mut file).map_err(|why| format!("{}: {why}", path.display()))?;
    let slot = table.slot(table.marked).expect("a table marks a slot it carries");
    let (start, len) = partition_extent(&mut file, slot.root).map_err(|why| format!("{}: {why}", path.display()))?;
    let len = usize::try_from(len).map_err(|_| format!("a {len}-byte ROOT on {}", path.display()))?;
    let mut volume = vec![0u8; len];
    file.seek(SeekFrom::Start(start))
        .and_then(|_| file.read_exact(&mut volume))
        .map_err(|e| format!("reading {}'s ROOT at byte {start}: {e}", path.display()))?;
    let uuid = root_uuid_of(&volume);
    let fs = bcachefs::Mounted::<_, bcachefs::ReadOnly>::open(VecBlockIO::from_vec(volume))
        .map_err(|e| format!("{}'s ROOT does not mount: {e:?}", path.display()))?;
    let bytes = fs.read_file(name).map_err(|e| format!("{}'s ROOT: reading {name}: {e:?}", path.display()))?;
    Ok((uuid, bytes))
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
    let mut fs = Fat32::mount(VolumeIo(&mut volume)).map_err(|e| format!("the volume does not mount: {e}"))?;
    let mut found = fs.open(name).map_err(|e| format!("the volume has no {name}: {e}"))?;
    let mut bytes = vec![0u8; usize::try_from(found.len()).unwrap_or(usize::MAX)];
    fs.read(&mut found, 0, &mut bytes).map_err(|e| format!("reading {name}: {e}"))?;
    Ok(bytes)
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
pub(crate) fn one_partition_of(
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
pub(crate) const LBA: u32 = 512;

fn round_up_sectors(n: usize) -> usize {
    n.div_ceil(SECTOR) * SECTOR
}

/// Where each partition is made to start.
///
/// A correctness requirement rather than tidiness. Every block service
/// transfers whole 4 KiB blocks and each file server keeps its own cached
/// copies of the blocks it has touched (`userland/fileserver`); two partitions
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

/// The volume being built, as [`toyos_fat32`] sees it.
///
/// Byte-addressed with no block size of its own, because there is none: the
/// volume is a `Vec` in this process. The kernel's adapter is where the
/// read-modify-write against 4 KiB device blocks lives, and `toyos-fat32`'s own
/// host suite is where that shape is exercised.
pub(crate) struct VolumeIo<'a>(pub(crate) &'a mut [u8]);

impl VolumeIo<'_> {
    fn range(&self, offset: u64, len: usize) -> Result<core::ops::Range<usize>, IoError> {
        let start = usize::try_from(offset).map_err(|_| IoError::Device)?;
        let end = start.checked_add(len).ok_or(IoError::Device)?;
        if end > self.0.len() {
            return Err(IoError::Device);
        }
        Ok(start..end)
    }
}

impl BlockAccess for VolumeIo<'_> {
    fn capacity(&self) -> u64 {
        self.0.len() as u64
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), IoError> {
        let at = self.range(offset, buf.len())?;
        buf.copy_from_slice(&self.0[at]);
        Ok(())
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> Result<(), IoError> {
        let at = self.range(offset, buf.len())?;
        self.0[at].copy_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), IoError> {
        Ok(())
    }
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
    let mut fs = Fat32::mount(VolumeIo(volume))
        .unwrap_or_else(|e| panic!("the freshly formatted {label} volume does not mount: {e}"));
    for (path, data) in files {
        if let Some((dir, _)) = path.rsplit_once('/') {
            fs.create_dir_all(dir, time)
                .unwrap_or_else(|e| panic!("creating {dir}/ on {label}: {e}"));
        }
        let mut file = fs
            .create(path, time)
            .unwrap_or_else(|e| panic!("creating {path} on {label}: {e}"));
        fs.write(&mut file, 0, data)
            .unwrap_or_else(|e| panic!("writing {} bytes to {path} on {label}: {e}", data.len()));
        fs.flush_meta(&mut file, time)
            .unwrap_or_else(|e| panic!("recording {path} on {label}: {e}"));
    }
    fs.free_bytes()
        .unwrap_or_else(|e| panic!("counting the {label} volume's free clusters: {e}"));
    fs.sync().unwrap_or_else(|e| panic!("syncing the {label} volume: {e}"));
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
            // Mirrored in `bootloader/src/main.rs` as `\toyos\log.guid`, which
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

/// A slot's FAT volume of `bytes`: the kernel, its boot parameter and the
/// signed header naming both and ROOT (`toyos_update::slots`), or nothing for
/// a slot no image has been installed in yet.
///
/// The parameter is ROOT's name and then the actuators this boot arms,
/// comma-separated: handed to the kernel in `KernelArgs`, because the earliest
/// actuator fires before `mm::init` and there is nowhere later to fetch it
/// from. [`params_of`] is how the host asks a finished image which of them a
/// guest booting it would arm.
fn create_slot_volume(image: Option<&Sections>, bytes: usize) -> Vec<u8> {
    let mut volume = format_fat32(bytes, "TOYOS-SLOT");
    let files: Vec<(&str, &[u8])> = match image {
        Some(s) => vec![
            (toyos_update::slots::KERNEL_FILE, &s.kernel),
            (toyos_update::slots::CMDLINE_FILE, &s.cmdline),
            (toyos_update::slots::SIGNED_FILE, &s.signed),
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
    let starts = lay(&mut file, len, &[data_row(crate::gptwrite::random_guid(), data_bytes)]);
    write_at(&mut file, starts[0], &designation(data_bytes / SECTOR as u64), "DATA partition's designation");
    (starts[0], data_bytes)
}

/// A TOYOS-DATA partition's entry.
fn data_row(guid: Guid, len: u64) -> Row {
    Row { what: "DATA partition".into(), name: "ToyOS data".into(), kind: Guid::TOYOS_DATA, guid, len }
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
            let name = name_of(image, part.index());
            let at = part.first_lba() as usize * LBA as usize;
            let bytes = &image[at..at + part.lba_count().get() as usize * LBA as usize];
            (Row { what: format!("installed {name}"), name, kind, guid, len: bytes.len() as u64 }, bytes)
        })
        .collect();

    let (mut rows, volumes): (Vec<Row>, Vec<&[u8]>) = carried.into_iter().unzip();
    rows.insert(0, data_row(crate::gptwrite::random_guid(), data_bytes));
    let total = disk_bytes(rows.iter().map(|row| row.len));

    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(disk)
        .unwrap_or_else(|e| panic!("{} is not installed on, being no file this install made: {e}", disk.display()));
    file.set_len(total).unwrap_or_else(|e| panic!("size {} to {total} bytes: {e}", disk.display()));

    let starts = lay(&mut file, total, &rows);
    write_at(&mut file, starts[0], &designation(data_bytes / SECTOR as u64), "DATA partition's designation");
    for ((row, volume), start) in rows[1..].iter().zip(volumes).zip(&starts[1..]) {
        for (i, block) in volume.chunks(PARTITION_ALIGN).enumerate().filter(|(_, block)| block.iter().any(|byte| *byte != 0)) {
            write_at(&mut file, start + (i * PARTITION_ALIGN) as u64, block, &row.what);
        }
    }
}

/// The name `image`'s table gives its entry `index`, which `toyos-gpt` does
/// not read: the header at block 1 places the array and sizes its entries,
/// and a name is the UTF-16 units from byte 56 of one, up to the first zero.
fn name_of(image: &[u8], index: u32) -> String {
    let field = |at: usize, len: usize| &image[LBA as usize + at..][..len];
    let array = u64::from_le_bytes(field(72, 8).try_into().expect("eight bytes")) as usize * LBA as usize;
    let size = u32::from_le_bytes(field(84, 4).try_into().expect("four bytes")) as usize;
    let entry = &image[array + index as usize * size..][..size];
    let units: Vec<u16> =
        entry[56..128].chunks(2).map(|unit| u16::from_le_bytes([unit[0], unit[1]])).take_while(|unit| *unit != 0).collect();
    String::from_utf16(&units).unwrap_or_else(|e| panic!("partition {index}'s name is no UTF-16: {e}"))
}

/// Block 0 of a volume: the magic and its block count.
fn designation(blocks: u64) -> [u8; SECTOR] {
    let mut block = [0u8; SECTOR];
    block[..bcachefs::DESIGNATION_MAGIC.len()].copy_from_slice(&bcachefs::DESIGNATION_MAGIC);
    let at = bcachefs::DESIGNATION_BLOCKS_OFFSET;
    block[at..at + 8].copy_from_slice(&blocks.to_le_bytes());
    block
}

/// A disk file as logical blocks, for a reader that may not hold the image.
pub(crate) struct FileSectors<'a>(pub(crate) &'a mut std::fs::File);

impl toyos_gpt::Sectors for FileSectors<'_> {
    fn lba_bytes(&self) -> u32 {
        LBA
    }

    fn lba_count(&self) -> u64 {
        self.0.metadata().map(|m| m.len()).unwrap_or(0) / u64::from(LBA)
    }

    fn lba_count_granularity(&self) -> NonZeroU64 {
        NonZeroU64::new(1).expect("one is not zero")
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
    let entries: Vec<crate::gptwrite::Entry<'_>> = rows
        .iter()
        .zip(&starts)
        .map(|(row, start)| crate::gptwrite::Entry {
            kind: row.kind,
            unique: row.guid,
            first_lba: start / lba,
            last_lba: (start + row.len) / lba - 1,
            name: &row.name,
        })
        .collect();
    let table = crate::gptwrite::table(total / lba, crate::gptwrite::random_guid(), &entries);
    write_at(disk, 0, &table.primary, "partition table");
    write_at(disk, total - table.backup.len() as u64, &table.backup, "backup partition table");
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

    fn lba_count_granularity(&self) -> NonZeroU64 {
        NonZeroU64::new(1).expect("one is not zero")
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

/// Why the image at `disk` may not be published, or `Ok` because readers that
/// did not write it agree it is sound.
///
/// `toyos-gpt` finds each partition by the unique GUID the table claims for it,
/// then `toyos-fat32-check` judges a FAT volume against fatgen103 and
/// `bcachefs`'s mount path judges ROOT, so no writer defect is waved through by
/// its own judge. A volume the table misplaces fails its format check on
/// whatever it does land on, which is why the extents are not compared
/// separately.
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
                bcachefs::Mounted::<_, bcachefs::ReadOnly>::open(VecBlockIO::from_vec(
                    volume.to_vec(),
                ))
                .map_err(|e| {
                    format!("bcachefs will not mount the {what} of the image this build wrote: {e:?}")
                })?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ROOT image with one file in it, for the tests that need a real one
    /// rather than a placeholder: the assembler reads its superblock.
    fn tiny_root() -> Vec<u8> {
        create_root_image(&[("bin/supervisor".to_string(), b"supervisor".to_vec())], &[], true)
    }

    /// A fixed key: what these tests judge is the image, not whose it is.
    fn key() -> crate::signing::Key {
        crate::signing::Key::throwaway_from([0x42; 32])
    }

    fn signing(key: &crate::signing::Key) -> Signing<'_> {
        Signing { key, version: 7 }
    }

    /// A file set whose spacious build's `next_alloc` lands one alignment
    /// unit's rounding away from `block_count` — the shape that made a second,
    /// from-scratch mkfs at the smaller size reseed its hashes into a btree
    /// one block bigger than the first build's slack, and panic with
    /// `NoSpace` on the last symlink. `trim_root` needs no second mkfs, so it
    /// keeps whatever tree the first build already proved fits.
    #[test]
    fn a_second_build_at_the_boundary_does_not_run_out() {
        let n = 80u32;
        let files: Vec<(String, Vec<u8>)> = (0..n)
            .map(|i| (format!("bin/prog{i:04}"), vec![(i % 251) as u8; 200_000 + i as usize * 17]))
            .collect();
        let symlinks: Vec<(String, String)> = (0..n)
            .map(|i| (format!("bin/alias{i:04}"), format!("/system/bin/prog{i:04}")))
            .collect();

        let image = create_root_image(&files, &symlinks, true);

        let sb = superblock_of(&image);
        assert_eq!(sb.block_count * 4096, image.len() as u64);
        assert!(sb.free_blocks < (PARTITION_ALIGN / 4096) as u64, "{} free blocks", sb.free_blocks);
        bcachefs::Mounted::<_, bcachefs::ReadOnly>::open(VecBlockIO::from_vec(image))
            .expect("a trimmed image mounts");
    }

    /// ROOT carries its contents and less than one alignment unit of free
    /// blocks: the loader reads every block of it and the kernel keeps them.
    #[test]
    fn root_carries_less_than_an_alignment_unit_of_free_blocks() {
        let files: Vec<(String, Vec<u8>)> =
            (0..8u8).map(|i| (format!("bin/f{i}"), vec![i; 3 << 20])).collect();
        let image = create_root_image(&files, &[], true);
        let sb = superblock_of(&image);
        assert_eq!(sb.block_count * 4096, image.len() as u64);
        assert!(sb.free_blocks < (PARTITION_ALIGN / 4096) as u64, "{} free blocks", sb.free_blocks);
    }

    /// The volumes this build writes break no rule of the format, and the
    /// gate is silence rather than sameness: a suite that could only ask the
    /// guest to add no *new* complaint hides every complaint the writer left.
    /// Here rather than in the boot suite because it needs no guest, no QEMU and
    /// no kernel — a claim about the writer, failing in seconds.
    #[test]
    fn the_volumes_this_build_writes_break_no_format_rule() {
        let key = key();
        let s = sections(b"kernel", &tiny_root(), "", signing(&key));
        for (what, volume) in [
            ("ESP", create_esp_volume(Arch::X86_64, b"bootloader", crate::gptwrite::random_guid())),
            ("slot volume", create_slot_volume(Some(&s), FAT32_MIN_BYTES)),
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
        let disk = create_boot_image(Arch::X86_64, b"kernel", b"bootloader", &root_image, "", signing(&key), None);
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

        // Both superblock copies, because one is the other's backup.
        let root_at = start_of(root);
        let mut broken = disk.clone();
        broken[root_at] ^= 0xff;
        broken[root_at + root_image.len() - 4096] ^= 0xff;
        let refusal = certify(&broken, &parts).expect_err("that volume is not bcachefs");
        assert!(refusal.contains("bcachefs will not mount"), "{refusal}");
    }

    /// The unique GUID the table drew for the one TOYOS-ROOT-typed partition.
    fn root_partition_guid_of(disk: &[u8]) -> toyos_gpt::Guid {
        only(disk, toyos_gpt::Guid::TOYOS_ROOT)
    }

    /// Every file on `volume`, sorted.
    fn files_of(mut volume: Vec<u8>) -> Vec<String> {
        let mut fs = Fat32::mount(VolumeIo(&mut volume)).expect("mount a volume we just built");
        let mut found: Vec<String> = fs
            .walk("", 64)
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
            files_of(create_esp_volume(Arch::X86_64, b"bootloader", crate::gptwrite::random_guid())),
            ["EFI/BOOT/BOOTx64.EFI", "toyos/log.guid"]
        );
        let key = key();
        let s = sections(b"kernel", &tiny_root(), "", signing(&key));
        assert_eq!(
            files_of(create_slot_volume(Some(&s), FAT32_MIN_BYTES)),
            [toyos_update::slots::CMDLINE_FILE, toyos_update::slots::SIGNED_FILE, toyos_update::slots::KERNEL_FILE]
        );
        assert!(files_of(create_slot_volume(None, FAT32_MIN_BYTES)).is_empty());
    }

    /// **The host's own reading of what the loader checks**: the marked slot's
    /// signed header verifies under the key the image was signed with and names
    /// the kernel, the boot parameter and ROOT the image carries, byte for
    /// byte; the table names the slot's partitions; and an update image for the
    /// same parts splits into the same header and sections. A second slot is
    /// present, empty, and not marked.
    #[test]
    fn an_image_s_marked_slot_is_signed_over_exactly_what_it_carries() {
        let key = key();
        let root = tiny_root();
        let dir = toyos_tmpdir::TempDir::new("image-slot");
        let path = dir.join("slotted.img");
        let disk = create_boot_image(Arch::X86_64, b"\x7fELF kernel", b"bootloader", &root, "sched-fast-health", signing(&key), Some(SecondSlot { root_bytes: 8 << 20 }));
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
        let (at, _) = partition_extent(&mut file, a.root).expect("slot A's ROOT");
        let on_disk = &disk[at as usize..at as usize + header.root().len as usize];
        assert_eq!(kernel, b"\x7fELF kernel");
        assert_eq!(on_disk, &root[..]);
        assert_eq!(header, toyos_update::image::Header::of(7, &kernel, &cmdline, on_disk));
        drop(file);
        drop(dir);

        let update = update_image(b"\x7fELF kernel", &root, "sched-fast-health", signing(&key));
        let parts = toyos_update::image::Parts::split(&update).expect("an update image splits");
        assert_eq!(parts.header, header);
        assert_eq!(toyos_update::image::Header::of(7, parts.kernel, parts.cmdline, parts.root), header);
        assert_eq!(parts.signed, &signed);
    }

    /// Every partition of `image` is on `disk` under its GUID, its name and with
    /// its bytes, found by the reader the kernel and `diskserver` use: the
    /// names, in the image's order.
    fn carries_every_partition_of(disk: &mut std::fs::File, image: &[u8]) -> Vec<String> {
        let mut table = vec![0u8; 34 * LBA as usize];
        disk.seek(SeekFrom::Start(0)).and_then(|_| disk.read_exact(&mut table)).expect("the disk's table");
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
            assert_eq!(name_of(&table, on_disk.index()), name_of(image, part.index()), "{}", part.unique_guid());
            names.push(name_of(image, part.index()));
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
        let image = create_boot_image(Arch::X86_64, b"kernel", b"bootloader", &tiny_root(), "", signing(&key), None);
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
        assert_eq!(stamp, designation(DATA / SECTOR as u64));
        drop(disk);

        let theirs = dir.join("theirs.img");
        std::fs::write(&theirs, b"somebody's").expect("write a file");
        let refused = std::panic::catch_unwind(|| install(&image, &theirs, DATA)).expect_err("an install on a file that exists");
        let said = refused.downcast_ref::<String>().expect("a refusal in words");
        assert!(said.contains("theirs.img"), "the refusal names no path: {said}");
        assert_eq!(std::fs::read(&theirs).expect("read it back"), b"somebody's");
    }

    /// **One ordering of one set is one image.** The judge below compares
    /// `root=` against the superblock the same build stamped, so it is blind to
    /// this: a `root_uuid` returning a constant satisfies it. This is the arm
    /// that is not — the same files handed over backwards have to come out as
    /// one UUID and one byte string, or two builds of one tree are two images
    /// and the kernel argument names a filesystem the next build has not got.
    #[test]
    fn one_ordering_of_one_set_is_one_image() {
        let files: Vec<(String, Vec<u8>)> = vec![
            ("bin/supervisor".to_string(), b"supervisor-binary".to_vec()),
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
            root_uuid_of(&forwards),
            root_uuid_of(&backwards),
            "one set of files named two filesystems"
        );
        assert_eq!(forwards.len(), backwards.len());
        assert!(
            forwards == backwards,
            "one set of files wrote two different {}-byte volumes under one name {}",
            forwards.len(),
            root_uuid_of(&forwards)
        );

        // And it is not a constant: a different set is a different name.
        let mut other = files;
        other[0].1.push(b'!');
        assert_ne!(
            root_uuid_of(&forwards),
            root_uuid_of(&create_root_image(&other, &symlinks, true)),
            "one byte of one file changed and the filesystem kept its name"
        );
    }

    /// **The independent oracle for ROOT**, asked of the finished image by
    /// readers that did not write it: `toyos-gpt` finds the partition by type
    /// where the table placed it, `bcachefs`'s mount-and-read path lists
    /// what its format-and-write path put there, and `toyos-fat32` reads the
    /// boot parameter off the ESP.
    ///
    /// Every name, size, content hash and symlink target against the list
    /// handed to [`create_root_image`] — a listing that merely parsed would
    /// pass a far weaker claim — and `root=` against the superblock UUID the
    /// reader found, which is the whole of how a boot picks its ROOT.
    #[test]
    fn the_root_partition_reads_back_as_the_files_the_build_put_in_it() {
        let files: Vec<(String, Vec<u8>)> = vec![
            ("bin/supervisor".to_string(), b"supervisor-binary".to_vec()),
            // Multi-block, so an extent list that stopped after the first is
            // caught by the hash rather than by the size.
            ("bin/toybox".to_string(), (0..40_000u32).map(|i| (i ^ 0x5A) as u8).collect()),
            ("etc/system.manifest".to_string(), b"[start]\nsupervisor\n".to_vec()),
            ("share/empty".to_string(), Vec::new()),
        ];
        let symlinks = vec![("bin/ls".to_string(), "/system/bin/toybox".to_string())];

        let root_image = create_root_image(&files, &symlinks, true);
        let key = key();
        let disk = create_boot_image(Arch::X86_64, b"kernel", b"bootloader", &root_image, "", signing(&key), None);

        // Located by *type*, through the parser the kernel uses, at the offset
        // the table gives — never at the one the writer computed.
        let part = only_partition(&mut ImageSectors(&disk), toyos_gpt::Guid::TOYOS_ROOT)
            .expect("a boot image carries exactly one TOYOS-ROOT partition");
        let at = part.first_lba() as usize * LBA as usize;
        let bytes = part.lba_count().get() as usize * LBA as usize;
        let volume = disk[at..at + bytes].to_vec();

        let fs = bcachefs::Mounted::<_, bcachefs::ReadOnly>::open(VecBlockIO::from_vec(volume))
            .expect("the ROOT partition mounts");

        let mut listed: Vec<(String, u64)> =
            fs.list(usize::MAX, &|_| true).expect("list the ROOT partition");
        listed.sort();
        let mut want: Vec<(String, u64)> = files
            .iter()
            .map(|(name, data)| (name.clone(), data.len() as u64))
            .chain(symlinks.iter().map(|(name, to)| (name.clone(), to.len() as u64)))
            .collect();
        want.sort();
        assert_eq!(listed, want, "ROOT does not hold the names and sizes it was built from");

        for (name, data) in &files {
            let read = fs.read_file(name).unwrap_or_else(|e| panic!("read {name}: {e:?}"));
            assert_eq!(
                Sha256::digest(&read),
                Sha256::digest(data),
                "{name} reads back as {} bytes that are not the {} it was given",
                read.len(),
                data.len()
            );
        }
        for (name, target) in &symlinks {
            let read = fs
                .read_link(name, 4096)
                .unwrap_or_else(|e| panic!("read_link {name}: {e:?}"));
            assert_eq!(read.as_deref(), Some(target.as_str()));
        }

        // And the kernel argument names *this* filesystem: the parameter comes
        // off the ESP through the FAT driver, the UUID out of the superblock
        // the mount above read.
        let scratch = toyos_tmpdir::TempDir::new("root-oracle");
        let path = scratch.join("root-oracle.img");
        std::fs::write(&path, &disk).expect("stage the image");
        let cmdline = cmdline_of(&path).expect("the ESP carries a boot parameter");
        assert_eq!(
            toyos_abi::boot::root_uuid(&cmdline),
            Some(fs.uuid().to_string().as_str()),
            "the boot parameter {cmdline:?} does not name the ROOT this image carries"
        );
    }

    /// **The independent oracle for the hierarchy.** ROOT mounts at `/system`, so
    /// every absolute path the shipped image declares is `/system/` plus a name
    /// ROOT carries, the two the kernel opens by hard-coded path included. Asked
    /// of a finished image by readers that did not write it: `toyos-gpt` finds
    /// ROOT by type where the table says, `bcachefs` mounts it, and
    /// `toyos-manifest` parses the records out of the volume.
    ///
    /// `SUPERVISOR_PATH` comes from the kernel source, and that is a **text scan of
    /// one spelling** — the `pub const SUPERVISOR_PATH: &str = "…";` line — so a
    /// kernel computing its spawn path otherwise is walked past. Not finding
    /// the line fails here rather than skipping.
    #[test]
    fn every_declared_path_resolves_inside_the_root_partition() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let (manifest, symlinks) = crate::build::manifest_and_symlinks(&root.join("system.toml"));
        let declared = toyos_manifest::parse(
            std::str::from_utf8(&manifest).expect("the manifest this build renders is text"),
        );

        // Placeholders: what this judges is the path scheme, and the bytes of a
        // real binary say nothing about it.
        let mut files: Vec<(String, Vec<u8>)> = declared
            .programs
            .iter()
            .map(|p| (under_system(&p.path).to_string(), b"placeholder".to_vec()))
            .collect();
        files.push(("bin/supervisor".to_string(), b"supervisor".to_vec()));
        files.push((toyos_manifest::PATH.to_string(), manifest.clone()));

        let disk = create_boot_image(
            Arch::X86_64,
            b"kernel",
            b"bootloader",
            &create_root_image(&files, &symlinks, true),
            "",
            signing(&key()),
            None,
        );

        let part = only_partition(&mut ImageSectors(&disk), toyos_gpt::Guid::TOYOS_ROOT)
            .expect("a boot image carries exactly one TOYOS-ROOT partition");
        let at = part.first_lba() as usize * LBA as usize;
        let bytes = part.lba_count().get() as usize * LBA as usize;
        let fs = bcachefs::Mounted::<_, bcachefs::ReadOnly>::open(VecBlockIO::from_vec(
            disk[at..at + bytes].to_vec(),
        ))
        .expect("the ROOT partition mounts");

        // As the volume holds it, not as the renderer returned it.
        let on_root = fs
            .read_file(toyos_manifest::PATH)
            .expect("ROOT carries the manifest where the guest path names it");
        assert_eq!(on_root, manifest, "the manifest on ROOT is not the one the build rendered");
        assert_eq!(
            toyos_manifest::GUEST_PATH,
            format!("/system/{}", toyos_manifest::PATH),
            "the path a process opens is not where ROOT carries the manifest"
        );

        let supervisor = supervisor_path_of_the_kernel(root);
        let names: std::collections::BTreeSet<String> = fs
            .list(usize::MAX, &|_| true)
            .expect("list the ROOT partition")
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        for path in std::iter::once(&supervisor).chain(declared.programs.iter().map(|p| &p.path)) {
            let name = under_system(path);
            assert_ne!(name, path.as_str(), "{path} is not under the mount ROOT gets");
            assert!(names.contains(name), "ROOT has no {name} for the declared path {path}");
        }
        assert!(names.contains("bin/supervisor"), "ROOT carries {names:?} and no bin/supervisor");
    }

    /// A declared absolute path as ROOT carries it; unchanged off that mount.
    fn under_system(path: &str) -> &str {
        path.strip_prefix("/system/").unwrap_or(path)
    }

    /// The literal in `kernel/src/loader/mod.rs`'s `SUPERVISOR_PATH`.
    fn supervisor_path_of_the_kernel(root: &Path) -> String {
        const ITEM: &str = "pub const SUPERVISOR_PATH: &str = \"";
        let source = std::fs::read_to_string(root.join("kernel/src/loader/mod.rs"))
            .expect("kernel/src/loader/mod.rs");
        source
            .lines()
            .find_map(|line| line.trim_start().strip_prefix(ITEM)?.split('"').next())
            .expect("no `pub const SUPERVISOR_PATH: &str = \"…\";` in kernel/src/loader/mod.rs")
            .to_string()
    }
}
