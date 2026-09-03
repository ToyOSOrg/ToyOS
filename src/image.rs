use std::collections::BTreeMap;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::num::NonZeroU64;
use std::path::Path;

use bcachefs::{Formatted, FsUuid, VecBlockIO};
use sha2::{Digest, Sha256};
use toyos_fat32::{BlockAccess, Fat32, FatTime, IoError};

/// The image that goes on the ROOT partition, named by a UUID derived from its
/// own contents.
///
/// **Derived, never drawn**: two builds of one tree have to agree on the name
/// the kernel argument then carries, and a random UUID would make the image a
/// different image on every build. The digest is over what this was given, so
/// it names those files and those symlinks and nothing else.
pub fn create_root_image(
    files: &[(String, Vec<u8>)],
    symlinks: &[(String, String)],
    quiet: bool,
) -> Vec<u8> {
    let data_size: usize = files.iter().map(|(_, d)| d.len()).sum::<usize>();
    let total_entries = files.len() + symlinks.len();
    // Estimate: superblock(1) + bitmap + btree nodes + data blocks + backup(1) + 10% padding
    let data_blocks = data_size.div_ceil(4096);
    let btree_blocks = (total_entries / 30).max(2);
    let overhead = 64;
    let total_blocks = (1 + overhead + btree_blocks + data_blocks) * 11 / 10;
    // Whole alignment units: the volume is a GPT partition now, and
    // `Superblock::check` refuses a superblock whose block count is not its
    // view's exactly — so a partitioner rounding the size up to the alignment
    // would leave an image nothing can mount.
    let total_blocks = align_up(total_blocks.max(64), PARTITION_ALIGN / 4096) as u64;

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

/// A name for exactly this set of files and symlinks.
///
/// Lengths go into the digest beside the bytes, so no two entries can run
/// together into an input a different split would also produce.
fn root_uuid(files: &[(String, Vec<u8>)], symlinks: &[(String, String)]) -> FsUuid {
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

/// The name the ROOT image `bytes` carries, read back out of its superblock.
///
/// The stamp and the kernel argument come from one place this way: the argument
/// says what the image says, not what whoever assembled it meant to stamp.
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
    // ROOT is the one exception, and by design: its *type* selects candidates
    // and its superblock's UUID picks the one, because a release puts several
    // ROOTs on one disk and the bootloader chooses between them by writing this
    // argument.
    let cmdline = cmdline_with_root(root_uuid_of(root_bytes), params);
    let esp_volume = create_esp_volume(kernel_bytes, bl_bytes, log_guid, &cmdline);
    let log_volume = create_log_volume();
    create_gpt_disk(esp_volume, root_bytes, log_volume, log_guid)
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
const LBA: u32 = 512;

fn round_up_sectors(n: usize) -> usize {
    n.div_ceil(SECTOR) * SECTOR
}

/// Where each partition is made to start.
///
/// A correctness requirement rather than tidiness. The kernel's `BlockDevice`
/// transfers whole 4 KiB blocks and each mounted volume keeps its own resident
/// copies of the blocks it has touched (`fat32_adapter::FatDevice`); two
/// partitions sharing one device block would make each other's copies stale
/// with nothing able to notice. Unaligned, the ESP ended 1024 bytes into a
/// device block that the log partition then began in.
///
/// 1 MiB rather than the 4096 the kernel needs, because that is what every
/// partitioner uses and what a flash device's erase block wants.
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
/// The `64/63` beside this at the one call site is headroom for the
/// filesystem's own metadata, and it is why this cannot be the flat slack it
/// used to be. `fatfs` picks the cluster size from the volume size, and at the
/// 512-byte clusters it gives the smaller ones the two FAT copies cost one
/// byte of table per 64 bytes of volume — 4.1 MiB of the test image's 257 MiB,
/// more than the 4 MiB the size used to add. So the slack was entirely
/// metadata, and what a guest could write was whatever rounding happened to
/// leave: measured before the change at **48,640 bytes**, against `esp_files`'
/// own 41,097-byte blob. One more guest test binary in the initrd took it
/// negative, and the symptom is an fsync that fails while the host-side volume
/// still reports megabytes free.
///
/// `64/63` is the 512-byte-cluster worst case, so where `fatfs` picks larger
/// clusters it over-provisions rather than under: the same image measures
/// **7,901,184 bytes** free after the change, on 4096-byte clusters.
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
/// the volume, and that is the whole of the fix for a defect every image this
/// project ever produced carried: `fatfs`'s `create_dir` writes a long-name
/// entry ahead of each `.` and `..`, which the format requires to be a
/// subdirectory's first two entries, and gives `..` the root's cluster number
/// where the format requires zero. Twelve `fsck_msdos` complaints, present
/// before any guest ran, and neither reachable through that crate's API.
///
/// Using `toyos-fat32` is not a way round them. It deletes the second FAT32
/// writer from the project: this is the one the kernel appends `kernel.log`
/// with, the one `toyos-fat32`'s host suite runs the volume checker and a real
/// macOS mount against on every `cargo test` in that crate, and now the one the
/// claim "the image we build is clean" is a claim about. `fatfs` keeps the
/// format call, where it has never had a complaint against it — an empty volume
/// has no subdirectory for either bug to live in.
///
/// The free-cluster count is the third thing the checker asks for:
/// `format_volume` leaves FSInfo's field 0xFFFFFFFF, which FAT32 defines as
/// "unknown" and every host then reports this volume's free space from.
/// `free_bytes` counts the FAT when the volume arrived without a hint and
/// `sync` writes it.
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
/// log cannot use much of it: sixteen boots at `/bin/logd`'s `MAX_LOG_BYTES`
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

fn create_gpt_disk(
    esp_volume: Vec<u8>,
    root_volume: &[u8],
    log_volume: Vec<u8>,
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

    // The GUID `add_partition` drew for the log partition is discarded for the
    // one already written to the ESP. Both name the same partition and only one
    // of them can be chosen second.
    let mut table = gdisk.partitions().clone();
    table
        .get_mut(&log_id)
        .expect("the log partition was just added")
        .part_guid = log_guid;
    gdisk
        .update_partitions(table)
        .expect("failed to stamp the log partition's unique GUID");

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
    /// A bcachefs image; judged by the crate's *reader*, which is not the code
    /// that wrote it.
    Root,
}

/// Why the image at `disk` may not be published, or `Ok` because readers that
/// did not write it agree it is sound.
///
/// `toyos-gpt` finds each partition by the unique GUID the table claims for it,
/// then `toyos-fat32-check` judges a FAT volume's bytes against fatgen103 and
/// `bcachefs`'s mount path judges ROOT's, so no writer defect is waved through
/// by its own judge. A volume the table misplaces fails its format check on
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
    /// gate is silence rather than sameness.
    ///
    /// The ESP did break some, from the first image this project ever built
    /// until [`populate`] stopped writing it with `fatfs` — twelve complaints,
    /// before any guest ran, from two format violations that crate's
    /// `create_dir` has. The consequence was not only a dirty volume:
    /// `esp_filesystem` could only ask that the guest add no *new* complaint,
    /// so a complaint the guest produced for its own reason would have hidden
    /// inside the twelve.
    ///
    /// Here rather than in the boot suite because it needs no guest, no QEMU
    /// and no kernel: it is a claim about the writer, and it fails in seconds
    /// on `cargo test --lib`.
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
        let disk = create_gpt_disk(esp, &root_image, create_log_volume(), log_uuid);
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
    #[test]
    fn the_esp_carries_what_the_bootloader_looks_for() {
        let mut esp = create_esp_volume(b"kernel", b"bootloader", uuid::Uuid::new_v4(), "");
        let mut fs = Fat32::mount(VolumeIo(&mut esp)).expect("mount the ESP we just built");
        let found: Vec<String> =
            fs.walk("", 64).expect("walk the ESP").into_iter().map(|(path, _)| path).collect();
        for want in
            ["EFI/BOOT/BOOTx64.EFI", "toyos/kernel.elf", "toyos/log.guid", "toyos/cmdline"]
        {
            assert!(found.iter().any(|p| p.trim_start_matches('/') == want), "{want} is not on the ESP; it holds {found:?}");
        }
        assert!(
            !found.iter().any(|p| p.ends_with("initrd.img")),
            "the ESP still carries an image the bootloader no longer loads: {found:?}"
        );
    }

    /// An image says which actuators a guest booting it would arm, and the
    /// answer comes out of the image rather than from whoever built it.
    ///
    /// **This is what makes a staged boot image answerable.** The actuators are
    /// baked in at build time, so a caller that supplies its own image cannot
    /// arm anything by asking — and until [`param_conflict`] existed nothing
    /// refused the pair: the harness took the parameters, built a kernel for
    /// them, booted the supplied image instead, and reported the arm as taken.
    /// A green run with an inert arm is the worst kind of harness defect,
    /// because every negative control staged through one proves nothing.
    ///
    /// Both directions, over images this file's own writer produced: the reader
    /// is the writer's inverse and a round trip is the only thing that says so.
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

        // The recorded defect, 2026-08-22: an actuator armed beside an image
        // built without it. It has to name both sides — the reader gets the
        // message and nothing else.
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

    /// **The independent oracle for ROOT.** Everything about the root
    /// filesystem this build wrote, asked of the finished image by readers that
    /// did not write it: `toyos-gpt` finds the partition by type where the
    /// `gpt` crate placed it, `bcachefs`'s mount-and-read path lists what its
    /// format-and-write path put there, and `toyos-fat32` reads the boot
    /// parameter off the ESP that `fatfs` formatted.
    ///
    /// Every name, every size, every content hash and every symlink target,
    /// against the list handed to [`create_root_image`] — a listing that merely
    /// parsed would pass a much weaker claim — and then `root=` against the
    /// superblock UUID the reader found, which is the whole of how a boot picks
    /// its ROOT.
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
        let symlinks = vec![("bin/ls".to_string(), "/bin/toybox".to_string())];

        let root_image = create_root_image(&files, &symlinks, true);
        let disk = create_boot_image(b"kernel", b"bootloader", &root_image, "");

        // Located by *type*, through the parser the kernel uses, at the offset
        // the table gives — never at the offset the writer computed.
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

        // And the kernel argument names *this* filesystem: the boot parameter
        // comes off the ESP through the FAT driver, the UUID out of the
        // superblock the mount above read.
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
}
