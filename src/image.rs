use std::collections::BTreeMap;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::num::NonZeroU64;
use std::path::Path;

use bcachefs::{Formatted, FsUuid, VecBlockIO};
use sha2::{Digest, Sha256};
use toyos_fat32::{BlockAccess, Fat32, FatTime, IoError};

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
    // Whole alignment units: `Superblock::check` refuses a superblock whose
    // block count is not its view's exactly, so a partitioner rounding the
    // size up to the alignment would leave an image nothing can mount.
    let total_blocks = align_up(total_blocks.max(64), PARTITION_ALIGN / 4096) as u64;

    let io = VecBlockIO::new(total_blocks);
    let mut fs = Formatted::format(io).expect("format an in-memory image");

    for (name, data) in &files {
        if !quiet {
            eprintln!("root: adding '{}' ({} bytes)", name, data.len());
        }
        fs.create(name, data, 0)
            .unwrap_or_else(|e| panic!("root: failed to add '{}': {:?}", name, e));
    }

    for (name, target) in &symlinks {
        if !quiet {
            eprintln!("root: symlink '{}' -> '{}'", name, target);
        }
        fs.create_symlink(name, target, 0)
            .unwrap_or_else(|e| panic!("root: failed to symlink '{}' -> '{}': {:?}", name, target, e));
    }

    fs.set_uuid(root_uuid(&files, &symlinks));
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
    let block = <[u8; 4096]>::try_from(&bytes[..4096])
        .expect("a bcachefs image is at least one block");
    bcachefs::Superblock::parse(&bcachefs::BlockBuf(block))
        .expect("the ROOT image this build wrote carries a superblock")
        .uuid
}

/// Takes the artifacts as bytes rather than reading them: the caller stages them
/// under a build-key-derived name first, because cargo's own path is shared by
/// every config and is overwritten by any concurrent build (see `build.rs`).
pub fn create_boot_image(
    kernel_bytes: &[u8],
    bl_bytes: &[u8],
    root_bytes: &[u8],
    params: &str,
) -> Vec<u8> {
    // Drawn here and written twice: into the GPT entry that *is* the log
    // partition, and into a file on the ESP that the bootloader hands the
    // kernel. The kernel is given the partition by name; nothing anywhere goes
    // looking for one by type or by format.
    let log_guid = uuid::Uuid::new_v4();
    let esp_guid = uuid::Uuid::new_v4();
    // ROOT is the one exception: its *type* selects candidates and its
    // superblock's UUID picks one, because a release puts several ROOTs on one
    // disk and the bootloader chooses by writing this argument.
    let cmdline = cmdline_with_root(root_uuid_of(root_bytes), params);
    let esp_volume = create_esp_volume(kernel_bytes, bl_bytes, log_guid, &cmdline);
    let log_volume = create_log_volume();
    create_gpt_disk(esp_volume, root_bytes, log_volume, esp_guid, log_guid)
}

/// The boot parameter as [`CMDLINE`] carries it: ROOT's name, then `params`.
fn cmdline_with_root(root: FsUuid, params: &str) -> String {
    if params.is_empty() {
        format!("root={root}")
    } else {
        format!("root={root},{params}")
    }
}

/// The file on the ESP that says which actuators an image is armed with.
///
/// Written by [`create_esp_volume`] and read back by [`params_of`], from this
/// one name: the writer and its inverse cannot drift apart while they share it.
/// The bootloader spells it `\toyos\cmdline`.
const CMDLINE: &str = "toyos/cmdline";

/// Why a boot may not arm `asked` on the image at `path`, or `None` because
/// that image is armed with exactly that list.
///
/// **An image carries the actuators it was built with**, in [`CMDLINE`] on its
/// own ESP — the file the bootloader reads and hands the kernel in
/// `KernelArgs`. So what a guest will be armed with is a fact about the image,
/// answerable before anything starts and without asking the guest; a caller
/// that has an image and a list can be told it is holding two different boots.
///
/// Pure, and every input a parameter, so both directions can be staged without
/// a guest — which is what `an_image_says_what_it_is_armed_with` does.
pub fn param_conflict(path: &Path, asked: &[&str]) -> Option<String> {
    let baked = match params_of(path) {
        Ok(baked) => baked,
        Err(why) => return Some(why),
    };
    if baked.iter().map(String::as_str).eq(asked.iter().copied()) {
        return None;
    }
    Some(format!(
        "the image {} is armed with {baked:?} and the boot asks for {asked:?}",
        path.display()
    ))
}

/// The actuator list an image is armed with, read back off the image.
///
/// `root=` is not an actuator and is on every image: this answers what a boot
/// would *arm*, which is the other reading of the same string
/// (`toyos_abi::boot::actuators`).
fn params_of(path: &Path) -> Result<Vec<String>, String> {
    let text = cmdline_of(path)?;
    Ok(toyos_abi::boot::actuators(&text).map(str::to_string).collect())
}

/// The whole boot parameter an image carries, read back off the image.
///
/// The ESP is located through the partition table rather than at the offset
/// [`create_gpt_disk`] happens to place it at: the writer asks `add_partition`
/// where the partition went, and so does this.
fn cmdline_of(path: &Path) -> Result<String, String> {
    let (start, len) = {
        let disk = gpt::GptConfig::new()
            .writable(false)
            .logical_block_size(gpt::disk::LogicalBlockSize::Lb512)
            .open(path)
            .map_err(|e| format!("{} has no readable GPT: {e}", path.display()))?;
        let esps: Vec<_> = disk
            .partitions()
            .values()
            .filter(|p| p.part_type_guid == gpt::partition_types::EFI)
            .collect();
        let [esp] = esps.as_slice() else {
            return Err(format!(
                "{} has {} ESPs, and a boot image has one",
                path.display(),
                esps.len()
            ));
        };
        (esp.first_lba * 512, (esp.last_lba - esp.first_lba + 1) * 512)
    };

    // The ESP alone, and never the whole image: a test image is a quarter of a
    // gigabyte and this runs once per boot that stages one.
    let len = usize::try_from(len).map_err(|_| format!("{} has a {len}-byte ESP", path.display()))?;
    let mut volume = vec![0u8; len];
    let mut file = std::fs::File::open(path)
        .map_err(|e| format!("opening {}: {e}", path.display()))?;
    file.seek(SeekFrom::Start(start))
        .map_err(|e| format!("seeking to the ESP of {}: {e}", path.display()))?;
    file.read_exact(&mut volume)
        .map_err(|e| format!("reading the ESP of {}: {e}", path.display()))?;

    // Read with the driver the kernel mounts this very volume with, rather than
    // with the crate that formatted it — [`populate`]'s argument, in the other
    // direction.
    let mut fs = Fat32::mount(VolumeIo(&mut volume))
        .map_err(|e| format!("the ESP of {} does not mount: {e}", path.display()))?;
    let mut file = fs
        .open(CMDLINE)
        .map_err(|e| format!("{} has no {CMDLINE} on its ESP: {e}", path.display()))?;
    let mut text = vec![0u8; usize::try_from(file.len()).unwrap_or(usize::MAX)];
    fs.read(&mut file, 0, &mut text)
        .map_err(|e| format!("reading {CMDLINE} from {}: {e}", path.display()))?;
    String::from_utf8(text)
        .map_err(|e| format!("{CMDLINE} on {} is not text: {e}", path.display()))
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
/// A correctness requirement rather than tidiness. The kernel's `BlockDevice`
/// transfers whole 4 KiB blocks and each mounted volume keeps its own resident
/// copies of the blocks it has touched (`fat32_adapter::FatDevice`); two
/// partitions sharing one device block would make each other's copies stale
/// with nothing able to notice. 1 MiB rather than the 4096 the kernel needs,
/// because that is what every partitioner uses and what an erase block wants.
const PARTITION_ALIGN: usize = 1024 * 1024;

/// The smallest volume there is a FAT32 for.
///
/// FAT32 *is* the format with at least 65,525 clusters, and `fatfs` gives a
/// volume this size 512-byte clusters — so the data area alone is 33.5 MiB
/// before the two FATs and the reserved sectors. Measured at exactly this size:
/// the format succeeds and `fsck_msdos` reports 68,551 free clusters.
const FAT32_MIN_BYTES: usize = 34 * 1024 * 1024;

/// What a guest can write into `/boot` after the three files are on it.
///
/// The `64/63` beside this at the one call site is headroom for the two FAT
/// copies, which cost a byte of table per 64 bytes of volume at the 512-byte
/// clusters `fatfs` gives a small one — the worst case, so a volume whose
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
/// showed as. `format_volume` writes it into both places the format keeps it,
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
    let mut volume = vec![0u8; bytes];
    fatfs::format_volume(
        Cursor::new(&mut volume),
        fatfs::FormatVolumeOptions::new()
            .fat_type(fatfs::FatType::Fat32)
            .volume_label(fat_label(label)),
    )
    .unwrap_or_else(|e| panic!("failed to format the {label} volume: {e}"));
    volume
}

/// The volume being built, as [`toyos_fat32`] sees it.
///
/// Byte-addressed with no block size of its own, because there is none: the
/// volume is a `Vec` in this process. The kernel's adapter is where the
/// read-modify-write against 4 KiB device blocks lives, and `toyos-fat32`'s own
/// host suite is where that shape is exercised.
struct VolumeIo<'a>(&'a mut [u8]);

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
/// Written with **our** FAT32 driver rather than with the crate that formatted
/// the volume: `fatfs`'s `create_dir` writes a long-name entry ahead of each
/// `.` and `..`, which the format requires to be a subdirectory's first two
/// entries, and gives `..` the root's cluster number where the format requires
/// zero — neither reachable through that crate's API. `toyos-fat32` is also the
/// driver the kernel appends `kernel.log` with and the one its own host suite
/// runs the volume checker against, so "the image we build is clean" is a claim
/// about the writer that is judged. `fatfs` keeps the format call, where an
/// empty volume has no subdirectory for either bug to live in.
///
/// `format_volume` leaves FSInfo's free-cluster field 0xFFFFFFFF, which FAT32
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

/// The partition firmware boots from: the bootloader, the kernel, the kernel's
/// arguments, and the name of the partition the kernel's log goes on.
fn create_esp_volume(
    kernel: &[u8],
    bootloader: &[u8],
    log_guid: uuid::Uuid,
    cmdline: &str,
) -> Vec<u8> {
    let content_size = kernel.len() + bootloader.len();
    let total_size = round_up_sectors(
        ((content_size + ESP_FREE_BYTES) * 64 / 63).max(FAT32_MIN_BYTES),
    );

    let mut volume = format_fat32(total_size, "TOYOS-BOOT");

    populate(
        &mut volume,
        "TOYOS-BOOT",
        &[
            ("EFI/BOOT/BOOTx64.EFI", bootloader),
            ("toyos/kernel.elf", kernel),
            // Mirrored in `bootloader/src/main.rs` as `\toyos\log.guid`, which
            // reads it beside the two files above and refuses the volume if it
            // is not there. The sixteen bytes are the GPT entry's own, in the
            // entry's own order: nothing converts them on the way to the kernel
            // and nothing converts the table's, so the comparison that decides
            // which partition holds the log cannot be got backwards.
            ("toyos/log.guid", &log_guid.to_bytes_le()),
            // ROOT's name and then the actuators this boot arms, comma-
            // separated. Read by the bootloader beside the two above and handed
            // to the kernel in `KernelArgs`, because the earliest actuator
            // fires before `mm::init` and there is nowhere later to fetch it
            // from. `kernel/src/actuator.rs` is the actuator list, and
            // [`params_of`] is how the host asks a finished image which of them
            // a guest booting it would arm.
            (CMDLINE, cmdline.as_bytes()),
        ],
    );

    volume
}

/// The partition the kernel's log lives on, empty until a machine boots.
///
/// Exactly [`FAT32_MIN_BYTES`], because the floor is not ours to choose and the
/// log cannot use much of it: sixteen boots at `/system/bin/logd`'s `MAX_LOG_BYTES`
/// come to 16 MiB, under half of what this volume has free, and there is no
/// smaller FAT32 to cut it down to.
fn create_log_volume() -> Vec<u8> {
    let mut volume = format_fat32(FAT32_MIN_BYTES, "TOYOS-LOG");
    populate(&mut volume, "TOYOS-LOG", &[]);
    volume
}

/// `B350BC93-…`, the TOYOS-ROOT partition type, as the writer names a type.
///
/// `toyos_gpt::Guid::TOYOS_ROOT` is the same sixteen bytes as the kernel
/// matches them, and `toyos_root_text_is_the_type_guid` is what keeps the two
/// spellings one constant.
const TOYOS_ROOT: gpt::partition_types::Type = gpt::partition_types::Type {
    guid: toyos_gpt::Guid::TOYOS_ROOT_TEXT,
    os: gpt::partition_types::OperatingSystem::None,
};

/// `064E3777-…`, the TOYOS-DATA partition type, the same way round.
const TOYOS_DATA: gpt::partition_types::Type = gpt::partition_types::Type {
    guid: toyos_gpt::Guid::TOYOS_DATA_TEXT,
    os: gpt::partition_types::OperatingSystem::None,
};

/// Lay a table on the disk at `path`, already `len` bytes long, carrying one
/// TOYOS-DATA partition, stamp the designation at its first block, and answer
/// where it landed. The table and the stamp are all this writes, so a sparse
/// file stays sparse. The stamp names that partition's own block count, so a
/// copy of this image designates no partition of another size.
pub fn designate_data_disk(path: &Path, len: u64) -> (u64, u64) {
    use std::io::Write;

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
    let mbr =
        gpt::mbr::ProtectiveMBR::with_lb_size(u32::try_from(len / 512 - 1).unwrap_or(0xFF_FF_FF_FF));
    mbr.overwrite_lba0(&mut file).expect("write the protective MBR");

    let mut gdisk = gpt::GptConfig::default()
        .initialized(false)
        .writable(true)
        .logical_block_size(gpt::disk::LogicalBlockSize::Lb512)
        .create_from_device(Box::new(file), None)
        .expect("create a GPT on the data disk");
    gdisk
        .update_partitions(BTreeMap::<u32, gpt::partition::Partition>::new())
        .expect("initialize the data disk's partition table");
    let id = gdisk
        .add_partition("ToyOS data", data_bytes, TOYOS_DATA, 0, Some((PARTITION_ALIGN / 512) as u64))
        .expect("add the data partition");
    let placed = gdisk.partitions().get(&id).expect("the partition was just added");
    let start = placed
        .bytes_start(gpt::disk::LogicalBlockSize::Lb512)
        .expect("the data partition's start");
    let bytes = placed
        .bytes_len(gpt::disk::LogicalBlockSize::Lb512)
        .expect("the data partition's length");
    assert_eq!(start % SECTOR as u64, 0, "the data partition starts at byte {start}");
    assert_eq!(bytes % SECTOR as u64, 0, "the data partition is {bytes} bytes");

    let mut device = gdisk.write().expect("write the data disk's GPT");
    device.seek(SeekFrom::Start(start)).expect("seek to the data partition");
    device.write_all(&designation(bytes / SECTOR as u64)).expect("stamp the data partition");
    device.flush().expect("flush the data disk");
    (start, bytes)
}

/// Block 0 of a volume the kernel may format: the magic and its block count.
fn designation(blocks: u64) -> [u8; SECTOR] {
    let mut block = [0u8; SECTOR];
    block[..bcachefs::DESIGNATION_MAGIC.len()].copy_from_slice(&bcachefs::DESIGNATION_MAGIC);
    let at = bcachefs::DESIGNATION_BLOCKS_OFFSET;
    block[at..at + 8].copy_from_slice(&blocks.to_le_bytes());
    block
}

/// Where the one TOYOS-DATA partition on `path` is, by the parser the kernel
/// selects it with.
pub fn data_partition_of(path: &Path) -> Result<(u64, u64), String> {
    let mut file =
        std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut out = [BLANK_PARTITION; 2];
    let scan =
        toyos_gpt::locate_type(&mut FileSectors(&mut file), toyos_gpt::Guid::TOYOS_DATA, &mut out)
            .map_err(|e| format!("{} has no readable partition table: {e:?}", path.display()))?;
    if scan.matched != 1 {
        return Err(format!("{} carries {} TOYOS-DATA partitions", path.display(), scan.matched));
    }
    Ok((out[0].first_lba * u64::from(LBA), out[0].lba_count() * u64::from(LBA)))
}

/// A slot [`toyos_gpt::locate_type`] has not filled in.
const BLANK_PARTITION: toyos_gpt::Partition = toyos_gpt::Partition {
    index: 0,
    type_guid: toyos_gpt::Guid::ZERO,
    unique_guid: toyos_gpt::Guid::ZERO,
    first_lba: 0,
    last_lba: 0,
};

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

fn create_gpt_disk(
    esp_volume: Vec<u8>,
    root_volume: &[u8],
    log_volume: Vec<u8>,
    esp_guid: uuid::Uuid,
    log_guid: uuid::Uuid,
) -> Vec<u8> {
    // `add_partition` places each partition itself; this is the size the disk
    // has to be for it to have somewhere to put them — an aligned gap before
    // the ESP, an aligned gap between each pair, and one after the last for the
    // backup table.
    let root_at = align_up(PARTITION_ALIGN + esp_volume.len(), PARTITION_ALIGN);
    let log_at = align_up(root_at + root_volume.len(), PARTITION_ALIGN);
    let total_size = round_up_sectors(log_at + log_volume.len() + PARTITION_ALIGN);
    assert_eq!(total_size % 512, 0, "image must be a whole number of 512-byte sectors to be flashable");
    let mut disk = vec![0u8; total_size];

    let mut cursor = Cursor::new(&mut disk);

    let mbr = gpt::mbr::ProtectiveMBR::with_lb_size(
        u32::try_from((total_size / 512) - 1).unwrap_or(0xFF_FF_FF_FF),
    );
    mbr.overwrite_lba0(&mut cursor).expect("failed to write MBR");

    let mut gdisk = gpt::GptConfig::default()
        .initialized(false)
        .writable(true)
        .logical_block_size(gpt::disk::LogicalBlockSize::Lb512)
        .create_from_device(Box::new(cursor), None)
        .expect("failed to create GPT disk");

    gdisk
        .update_partitions(BTreeMap::<u32, gpt::partition::Partition>::new())
        .expect("failed to initialize partition table");

    let align = Some((PARTITION_ALIGN / 512) as u64);
    let esp_id = gdisk
        .add_partition("EFI System", esp_volume.len() as u64, gpt::partition_types::EFI, 0, align)
        .expect("failed to add ESP partition");
    // The one partition selected by *type*: which ROOT a boot mounts is decided
    // by the UUID in its own superblock, against the `root=` on the ESP.
    let root_id = gdisk
        .add_partition("ToyOS root", root_volume.len() as u64, TOYOS_ROOT, 0, align)
        .expect("failed to add the root partition");
    // Microsoft Basic Data, and that type is the whole reason this is a second
    // partition at all: macOS never auto-mounts an EFI-typed partition and this
    // host refuses even a manual non-root mount of one, so a log on the ESP is
    // unreachable without the admin account. This type mounts in Finder, in
    // Windows and in Linux on plug-in, with nothing configured.
    let log_id = gdisk
        .add_partition("ToyOS log", log_volume.len() as u64, gpt::partition_types::BASIC, 0, align)
        .expect("failed to add the log partition");

    // The GUID `add_partition` drew for each of these is discarded: the log's
    // for the one already written to the ESP, and the ESP's own because a
    // firmware boot entry names a partition by GUID and every name is drawn once.
    let mut table = gdisk.partitions().clone();
    table.get_mut(&esp_id).expect("the ESP was just added").part_guid = esp_guid;
    table
        .get_mut(&log_id)
        .expect("the log partition was just added")
        .part_guid = log_guid;
    gdisk
        .update_partitions(table)
        .expect("failed to stamp the ESP's and the log partition's unique GUIDs");

    let start_of = |id: u32| {
        gdisk
            .partitions()
            .get(&id)
            .expect("a partition that was just added")
            .bytes_start(gpt::disk::LogicalBlockSize::Lb512)
            .expect("failed to get a partition's start") as usize
    };
    let esp_start = start_of(esp_id);
    let root_start = start_of(root_id);
    let log_start = start_of(log_id);

    // `Superblock::check` refuses a superblock whose block count is not its
    // device's, so a partition wider than the image it carries is one the
    // kernel cannot mount. Checked here rather than left to a boot: this is the
    // writer, and the failure there is a machine with no userland.
    let root_bytes = gdisk
        .partitions()
        .get(&root_id)
        .expect("the root partition was just added")
        .bytes_len(gpt::disk::LogicalBlockSize::Lb512)
        .expect("failed to get the root partition's length");
    assert_eq!(
        root_bytes,
        root_volume.len() as u64,
        "the table gives ROOT {root_bytes} bytes for a {}-byte image",
        root_volume.len()
    );

    let named = |id: u32| {
        toyos_gpt::Guid(
            gdisk
                .partitions()
                .get(&id)
                .expect("a partition that was just added")
                .part_guid
                .to_bytes_le(),
        )
    };
    let esp_guid = named(esp_id);
    let root_partition_guid = named(root_id);
    let log_partition_guid = named(log_id);

    // The invariant [`PARTITION_ALIGN`] exists for, checked rather than
    // assumed: the kernel mounts all three at once over one 4 KiB block
    // device, and a device block belonging to two volumes would be cached
    // twice.
    let placed = [
        ("ESP", esp_start, esp_volume.len()),
        ("root partition", root_start, root_volume.len()),
        ("log partition", log_start, log_volume.len()),
    ];
    for (what, start, len) in placed {
        assert_eq!(start % SECTOR, 0, "the {what} starts at byte {start}, off a {SECTOR}-byte block");
        assert_eq!(len % SECTOR, 0, "the {what} is {len} bytes, not whole {SECTOR}-byte blocks");
    }
    for (before, after) in placed.iter().zip(&placed[1..]) {
        assert!(
            before.1 + before.2 <= after.1,
            "the {} runs to {} and the {} starts at {}",
            before.0,
            before.1 + before.2,
            after.0,
            after.1
        );
    }

    let mut disk_device = gdisk.write().expect("failed to write GPT");

    disk_device.seek(std::io::SeekFrom::Start(0)).expect("failed to seek");
    let mut final_bytes = vec![0u8; total_size];
    disk_device.read_exact(&mut final_bytes).expect("failed to read disk");

    final_bytes[esp_start..esp_start + esp_volume.len()].copy_from_slice(&esp_volume);
    final_bytes[root_start..root_start + root_volume.len()].copy_from_slice(root_volume);
    final_bytes[log_start..log_start + log_volume.len()].copy_from_slice(&log_volume);

    certify(
        &final_bytes,
        &[
            ("ESP", esp_guid, Volume::Fat32),
            ("root partition", root_partition_guid, Volume::Root),
            ("log partition", log_partition_guid, Volume::Fat32),
        ],
    )
    .unwrap_or_else(|refusal| panic!("{refusal}"));

    final_bytes
}

/// A disk image as logical blocks, so a GPT parser can read it without a file.
struct ImageSectors<'a>(&'a [u8]);

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
        let at = located.partition.first_lba as usize * LBA as usize;
        let bytes = located.partition.lba_count() as usize * LBA as usize;
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
        create_root_image(&[("bin/init".to_string(), b"init".to_vec())], &[], true)
    }

    /// The two volumes this build writes break no rule of the format, and the
    /// gate is silence rather than sameness: a suite that could only ask the
    /// guest to add no *new* complaint hides every complaint the writer left.
    /// Here rather than in the boot suite because it needs no guest, no QEMU and
    /// no kernel — a claim about the writer, failing in seconds.
    #[test]
    fn the_volumes_this_build_writes_break_no_format_rule() {
        for (what, volume) in [
            ("ESP", create_esp_volume(b"kernel", b"bootloader", uuid::Uuid::new_v4(), "")),
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

    /// Publishing a flash target runs every reader over the assembled image, and
    /// a damaged one is refused by name: each mutation is staged on an image
    /// [`create_gpt_disk`] just certified, so each refusal is its own.
    #[test]
    fn a_damaged_image_is_refused_by_the_reader_that_caught_it() {
        let log_uuid = uuid::Uuid::new_v4();
        let esp = create_esp_volume(b"kernel", b"bootloader", log_uuid, "");
        let root_image = tiny_root();
        let disk =
            create_gpt_disk(esp, &root_image, create_log_volume(), uuid::Uuid::new_v4(), log_uuid);
        let log = toyos_gpt::Guid(log_uuid.to_bytes_le());
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
                .partition
                .first_lba as usize
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
        let mut out = [toyos_gpt::Partition {
            index: 0,
            type_guid: toyos_gpt::Guid::ZERO,
            unique_guid: toyos_gpt::Guid::ZERO,
            first_lba: 0,
            last_lba: 0,
        }; 2];
        let scan =
            toyos_gpt::locate_type(&mut ImageSectors(disk), toyos_gpt::Guid::TOYOS_ROOT, &mut out)
                .expect("the image has a partition table");
        assert_eq!(scan.matched, 1, "a boot image carries one ROOT");
        out[0].unique_guid
    }

    /// And it is clean because it is right, not because it is empty: a
    /// `populate` that wrote nothing at all would satisfy the gate above.
    /// Exactly these and nothing else, because an unnamed file on the ESP is
    /// one the volume pays for and nothing loads.
    #[test]
    fn the_esp_carries_what_the_bootloader_looks_for() {
        let mut esp = create_esp_volume(b"kernel", b"bootloader", uuid::Uuid::new_v4(), "");
        let mut fs = Fat32::mount(VolumeIo(&mut esp)).expect("mount the ESP we just built");
        let mut found: Vec<String> = fs
            .walk("", 64)
            .expect("walk the ESP")
            .into_iter()
            .map(|(path, _)| path.trim_start_matches('/').to_string())
            .filter(|path| !path.ends_with('/'))
            .collect();
        found.sort();
        assert_eq!(
            found,
            [
                "EFI/BOOT/BOOTx64.EFI",
                "toyos/cmdline",
                "toyos/kernel.elf",
                "toyos/log.guid",
            ]
        );
    }

    /// An image says which actuators a guest booting it would arm, and the
    /// answer comes out of the image rather than from whoever built it.
    ///
    /// **This is what makes a staged boot image answerable.** The actuators are
    /// baked in at build time, so a caller supplying its own image cannot arm
    /// anything by asking, and a green run with an inert arm is the worst kind
    /// of harness defect: every negative control staged through one proves
    /// nothing. Both directions, because the reader is the writer's inverse.
    #[test]
    fn an_image_says_what_it_is_armed_with() {
        let dir = std::env::temp_dir().join(format!("toyos-image-params-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        let root_image = tiny_root();
        let write = |name: &str, params: &str| {
            let path = dir.join(name);
            std::fs::write(&path, create_boot_image(b"kernel", b"bootloader", &root_image, params))
                .expect("write an image");
            path
        };

        // What every shipping image is: nothing armed at all.
        let shipping = write("shipping.img", "");
        // Two, because a reader that handed back the whole file as one name
        // would answer every one-actuator question correctly.
        let armed = write("armed.img", "usb-flush-fails,fat-boot-reads-fail");

        for (image, asked) in [
            (&shipping, &[][..]),
            (&armed, &["usb-flush-fails", "fat-boot-reads-fail"][..]),
        ] {
            assert_eq!(
                param_conflict(image, asked),
                None,
                "an image was refused the list it was built with: {asked:?}"
            );
        }

        // An actuator armed beside an image built without it names both sides:
        // the reader gets the message and nothing else.
        for (image, asked, name) in [
            (&shipping, &["usb-flush-fails"][..], "usb-flush-fails"),
            (&armed, &[][..], "fat-boot-reads-fail"),
            (&armed, &["usb-flush-fails"][..], "fat-boot-reads-fail"),
        ] {
            let why = param_conflict(image, asked)
                .unwrap_or_else(|| panic!("{asked:?} was accepted on {}", image.display()));
            assert!(
                why.contains(name),
                "the refusal does not name {name}, which is the whole of what it is about: {why}"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
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
            ("bin/init".to_string(), b"init-binary".to_vec()),
            ("bin/toybox".to_string(), (0..40_000u32).map(|i| (i ^ 0x5A) as u8).collect()),
            ("etc/system.manifest".to_string(), b"[start]\ninit\n".to_vec()),
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
    /// where the `gpt` crate placed it, `bcachefs`'s mount-and-read path lists
    /// what its format-and-write path put there, and `toyos-fat32` reads the
    /// boot parameter off the ESP that `fatfs` formatted.
    ///
    /// Every name, size, content hash and symlink target against the list
    /// handed to [`create_root_image`] — a listing that merely parsed would
    /// pass a far weaker claim — and `root=` against the superblock UUID the
    /// reader found, which is the whole of how a boot picks its ROOT.
    #[test]
    fn the_root_partition_reads_back_as_the_files_the_build_put_in_it() {
        let files: Vec<(String, Vec<u8>)> = vec![
            ("bin/init".to_string(), b"init-binary".to_vec()),
            // Multi-block, so an extent list that stopped after the first is
            // caught by the hash rather than by the size.
            ("bin/toybox".to_string(), (0..40_000u32).map(|i| (i ^ 0x5A) as u8).collect()),
            ("etc/system.manifest".to_string(), b"[start]\ninit\n".to_vec()),
            ("share/empty".to_string(), Vec::new()),
        ];
        let symlinks = vec![("bin/ls".to_string(), "/system/bin/toybox".to_string())];

        let root_image = create_root_image(&files, &symlinks, true);
        let disk = create_boot_image(b"kernel", b"bootloader", &root_image, "");

        // Located by *type*, through the parser the kernel uses, at the offset
        // the table gives — never at the one the writer computed.
        let mut out = [toyos_gpt::Partition {
            index: 0,
            type_guid: toyos_gpt::Guid::ZERO,
            unique_guid: toyos_gpt::Guid::ZERO,
            first_lba: 0,
            last_lba: 0,
        }; 4];
        let scan =
            toyos_gpt::locate_type(&mut ImageSectors(&disk), toyos_gpt::Guid::TOYOS_ROOT, &mut out)
                .expect("the image this build wrote has a partition table");
        assert_eq!(
            (scan.matched, scan.listed),
            (1, 1),
            "a boot image carries exactly one TOYOS-ROOT partition; this one has {}",
            scan.matched
        );
        let at = out[0].first_lba as usize * LBA as usize;
        let bytes = out[0].lba_count() as usize * LBA as usize;
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
        let path = std::env::temp_dir()
            .join(format!("toyos-root-oracle-{}.img", std::process::id()));
        std::fs::write(&path, &disk).expect("stage the image");
        let cmdline = cmdline_of(&path).expect("the ESP carries a boot parameter");
        let _ = std::fs::remove_file(&path);
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
    /// `INIT_PATH` comes from the kernel source, and that is a **text scan of
    /// one spelling** — the `pub const INIT_PATH: &str = "…";` line — so a
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
        files.push(("bin/init".to_string(), b"init".to_vec()));
        files.push((toyos_manifest::PATH.to_string(), manifest.clone()));

        let disk = create_boot_image(
            b"kernel",
            b"bootloader",
            &create_root_image(&files, &symlinks, true),
            "",
        );

        let mut out = [BLANK_PARTITION; 2];
        let scan =
            toyos_gpt::locate_type(&mut ImageSectors(&disk), toyos_gpt::Guid::TOYOS_ROOT, &mut out)
                .expect("the image this build wrote has a partition table");
        assert_eq!(scan.matched, 1, "a boot image carries exactly one TOYOS-ROOT partition");
        let at = out[0].first_lba as usize * LBA as usize;
        let bytes = out[0].lba_count() as usize * LBA as usize;
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

        let init = init_path_of_the_kernel(root);
        let names: std::collections::BTreeSet<String> = fs
            .list(usize::MAX, &|_| true)
            .expect("list the ROOT partition")
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        for path in std::iter::once(&init).chain(declared.programs.iter().map(|p| &p.path)) {
            let name = under_system(path);
            assert_ne!(name, path.as_str(), "{path} is not under the mount ROOT gets");
            assert!(names.contains(name), "ROOT has no {name} for the declared path {path}");
        }
        assert!(names.contains("bin/init"), "ROOT carries {names:?} and no bin/init");
    }

    /// A declared absolute path as ROOT carries it; unchanged off that mount.
    fn under_system(path: &str) -> &str {
        path.strip_prefix("/system/").unwrap_or(path)
    }

    /// The literal in `kernel/src/loader/mod.rs`'s `INIT_PATH`.
    fn init_path_of_the_kernel(root: &Path) -> String {
        const ITEM: &str = "pub const INIT_PATH: &str = \"";
        let source = std::fs::read_to_string(root.join("kernel/src/loader/mod.rs"))
            .expect("kernel/src/loader/mod.rs");
        source
            .lines()
            .find_map(|line| line.trim_start().strip_prefix(ITEM)?.split('"').next())
            .expect("no `pub const INIT_PATH: &str = \"…\";` in kernel/src/loader/mod.rs")
            .to_string()
    }
}
