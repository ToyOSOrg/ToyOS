#[repr(C)]
// No `Debug`: one `{:?}` of these would print the loader's seed.
#[derive(Clone, Copy)]
pub struct KernelArgs {
    pub memory_map_addr: u64,
    pub memory_map_size: u64,
    pub kernel_memory_addr: u64,
    pub kernel_memory_size: u64,
    pub kernel_stack_addr: u64,
    pub kernel_stack_size: u64,
    pub rsdp_addr: u64,
    pub kernel_elf_addr: u64,
    pub kernel_elf_size: u64,
    pub gop_framebuffer: u64,
    pub gop_framebuffer_size: u64,
    pub gop_width: u32,
    pub gop_height: u32,
    pub gop_stride: u32,
    pub gop_pixel_format: u32,
    /// Maps the low physical memory both at identity and at the high half.
    pub boot_pml4_addr: u64,
    /// First logical block of the partition the firmware loaded this image
    /// from, in that device's own block size.
    pub boot_partition_start_lba: u64,
    /// That partition's length, in the same blocks.
    ///
    /// Firmware's number, kept alongside the GUID so the kernel has two
    /// independent accounts of the partition's extent — this one and the GPT
    /// entry it finds. A disagreement means the table on the disk is not the
    /// table firmware read, and the kernel refuses rather than picking one.
    pub boot_partition_blocks: u64,
    /// The partition's *unique* GUID, exactly as it sits in the HARDDRIVE
    /// device path node and in the GPT entry — no byte order conversion on
    /// either side, so the comparison that decides which partition is ours
    /// cannot be got backwards.
    pub boot_partition_guid: [u8; 16],
    /// Zero when this machine has no designated boot partition, in which case
    /// the three fields above are zero as well.
    ///
    /// Not an error: booting over the network, or off a device with no
    /// partition table, is a machine ToyOS is expected to come up on. The
    /// kernel simply knows it has no partition it is entitled to write to.
    pub boot_partition_present: u32,
    /// The unique GUID of the partition the kernel's log goes on, read out of
    /// `\toyos\log.guid` on the volume the bootloader loaded itself from, in
    /// the same raw byte order as [`Self::boot_partition_guid`].
    ///
    /// No presence flag, unlike the boot partition above, and not because the
    /// state cannot arise but because it is not a machine. A machine really can
    /// have no boot partition to be named — PXE, an unpartitioned disk. But
    /// this GUID comes from a file `create_esp_volume` writes beside
    /// `kernel.elf`, so a volume carrying that one and not this one was not
    /// built by this project, and the bootloader refuses it by name rather than
    /// starting a kernel that would silently have nowhere to put its log.
    ///
    /// Naming the partition is all this does. Whether one with that GUID is on
    /// the disk is the kernel's question, and its answer there may well be no.
    pub log_partition_guid: [u8; 16],
    /// The layout the loader wrote this struct in: [`LAYOUT`] from a loader built
    /// with this file. The kernel refuses any other value before it reads a
    /// field after this one, so every field before it keeps its offset in every
    /// layout.
    pub layout: u32,
    /// The boot parameter, as ASCII with no terminator: comma-separated tokens
    /// read out of `\toyos\cmdline` on the volume the bootloader loaded itself
    /// from. [`root_uuid`] and [`actuators`] are the two readings of it.
    ///
    /// Every shipping image carries exactly `root=<uuid>`, and a kernel that
    /// carries no actuators refuses any other token rather than ignoring it
    /// (`kernel/src/actuator.rs`). It is in this struct rather than anywhere the
    /// kernel could go and fetch it because the earliest actuator panics before
    /// `mm::init` and another acts at AP bring-up: a parameter that is not here
    /// arrives too late to be one.
    ///
    /// Pool memory the bootloader forgets, and not in the kernel's reserved
    /// list, because it is parsed before `mm::init` runs and there is nothing
    /// left to protect.
    pub cmdline_addr: u64,
    pub cmdline_len: u64,
    /// Where the loader put ROOT: its filesystem, read through the firmware's
    /// block I/O into `LoaderData` pages, which the kernel mounts from memory and
    /// keeps out of its allocator by this address, as it keeps the black box's.
    ///
    /// These are the bytes a signature check over ROOT has to cover: the loader
    /// writes them once, and nothing writes them between that read and the
    /// kernel's mount. Zero length is a loader that handed no image, and the
    /// kernel refuses to boot on it by name; it never reads ROOT anywhere else.
    pub root_image_addr: u64,
    pub root_image_len: u64,
    /// The unique GUID of the partition the image was read from, raw as in the
    /// GPT entry like [`Self::boot_partition_guid`]; zero with no image. The
    /// kernel holds that partition so no claim writes the slot it is running.
    pub root_partition_guid: [u8; 16],
    /// The CPU's counter, the one the kernel's clock reads, at the loader's
    /// entry and at its handoff, and the ticks its read of ROOT took (zero with
    /// no image). The first is firmware's time since the counter started; the
    /// kernel converts all three at its clock's rate.
    pub loader_entry_counter: u64,
    pub loader_handoff_counter: u64,
    pub root_read_ticks: u64,
    /// What firmware's `EFI_RNG_PROTOCOL` gave the loader for the kernel's
    /// random generator, and how many of these bytes it is: [`SEED_LEN`], or
    /// zero where firmware has no such protocol or gave nothing usable, which
    /// is a machine and not an error. Secret: the kernel zeroes both fields
    /// once it has read them, and neither side prints a byte of them.
    pub loader_seed: [u8; SEED_LEN],
    pub loader_seed_len: u64,
    /// What firmware's `GetTime` (UEFI 2.11 §8.3.1) said the time was, in
    /// Unix seconds, and [`Self::loader_entry_counter`]'s counter as it
    /// answered: the instant and when it was true, so the kernel carries it on
    /// to its own boot exactly. UTC, because the hardware clock keeps UTC and
    /// `EFI_TIME::TimeZone` is not read.
    pub wall_clock_secs: u64,
    pub wall_clock_counter: u64,
    /// 1 where firmware answered a date that exists, and 0 where `GetTime`
    /// failed or answered one that does not, which is a machine and not an
    /// error: the loader's line says which, and the kernel has no wall clock.
    pub wall_clock_known: u64,
    /// The base block of the panel's EDID, unparsed, as firmware published it
    /// through `EFI_EDID_ACTIVE_PROTOCOL` on the one GOP handle that shows
    /// [`Self::gop_framebuffer`] in its mode, and how many of these bytes it
    /// is: [`EDID_BLOCK`], or zero where no such handle carries one. The bytes
    /// are firmware's, decoded by whoever reads them.
    pub gop_edid: [u8; EDID_BLOCK],
    pub gop_edid_len: u64,
    /// The microcode patch for this machine's CPU that the loader found on
    /// ROOT, read into `LoaderData` pages of its own: one update in its
    /// vendor's form (`toyos-microcode`), page-aligned, which the kernel
    /// keeps out of its allocator by this address. Zero length where ROOT
    /// holds none for the CPU, or the CPU's architecture loads none.
    pub microcode_addr: u64,
    pub microcode_len: u64,
}

/// The bytes of [`KernelArgs::gop_edid`]: one EDID base block.
pub const EDID_BLOCK: usize = 128;

/// The bytes of [`KernelArgs::loader_seed`].
pub const SEED_LEN: usize = 32;

/// [`KernelArgs::layout`] for the struct this file declares: the struct's own
/// size folded in. Never within -1440..=1440 as an `i32`: a loader older than
/// the word wrote a firmware zone in minutes at its offset.
pub const LAYOUT: u32 = 0x5459_0000 | core::mem::size_of::<KernelArgs>() as u32;

/// The boot parameter on which the loader writes 0 as [`KernelArgs::layout`],
/// what an older loader writes there on firmware that names no zone: the
/// negative control on the kernel's refusal, and read by both of them.
pub const WRITE_NO_LAYOUT_PARAM: &str = "loader-writes-no-layout";

/// The boot parameter on which the loader hands the kernel no ROOT image: the
/// negative control on the kernel's refusal, and read by both of them.
pub const WITHHOLD_ROOT_PARAM: &str = "loader-withholds-root";

/// The word the loader appends naming the slot it booted, `A` or `B`: the
/// signed boot parameter is the slot's own, and which slot carried it is a
/// fact only the loader has.
pub const SLOT_PARAM: &str = "boot-slot=";

/// The word the loader appends when it booted the other slot than the one the
/// slot table marks: the marked slot and why it was refused, `B:died`. The
/// kernel logs it, which is how a dead or refused boot reaches the next boot's
/// log.
pub const SLOT_REFUSED_PARAM: &str = "slot-refused=";

/// The token naming the filesystem the kernel mounts as root.
const ROOT_PARAM: &str = "root=";

/// What the boot parameter names ROOT, or `None` on a parameter that names none.
///
/// `bcachefs::FsUuid`'s text, compared against each candidate partition's
/// *superblock* and never against a partition GUID: a role names a filesystem,
/// and a filesystem may have members on more than one disk.
pub fn root_uuid(cmdline: &str) -> Option<&str> {
    cmdline.split(',').find_map(|token| token.strip_prefix(ROOT_PARAM))
}

/// Every token of the boot parameter that is not [`root_uuid`]'s. A token this
/// yields is one the actuator table has to declare, so a `root=` left in would
/// panic a kernel that boots.
pub fn actuators(cmdline: &str) -> impl Iterator<Item = &str> {
    cmdline.split(',').filter(|t| !t.is_empty() && !t.starts_with(ROOT_PARAM))
}


/// The kernel's `_start` reads three of these fields out of `rdi` by hardcoded
/// byte offset, before Rust code runs and before there is a stack to call a
/// getter on. Adding a field anywhere but the end moves them silently, and the
/// symptom is a stack pointer pointing at nothing.
///
/// The size and alignment are here for the other half of the contract: the
/// bootloader writes this struct and the kernel reads it, and the two are
/// separate binaries built for separate targets.
const _: () = {
    use core::mem::{align_of, offset_of, size_of};
    assert!(offset_of!(KernelArgs, kernel_memory_addr) == 16);
    assert!(offset_of!(KernelArgs, kernel_stack_addr) == 32);
    assert!(offset_of!(KernelArgs, kernel_stack_size) == 40);
    assert!(offset_of!(KernelArgs, boot_partition_start_lba) == 112);
    assert!(offset_of!(KernelArgs, boot_partition_blocks) == 120);
    assert!(offset_of!(KernelArgs, boot_partition_guid) == 128);
    assert!(offset_of!(KernelArgs, boot_partition_present) == 144);
    assert!(offset_of!(KernelArgs, log_partition_guid) == 148);
    assert!(offset_of!(KernelArgs, layout) == 164);
    assert!(offset_of!(KernelArgs, cmdline_addr) == 168);
    assert!(offset_of!(KernelArgs, cmdline_len) == 176);
    assert!(offset_of!(KernelArgs, root_image_addr) == 184);
    assert!(offset_of!(KernelArgs, root_image_len) == 192);
    assert!(offset_of!(KernelArgs, root_partition_guid) == 200);
    assert!(offset_of!(KernelArgs, loader_entry_counter) == 216);
    assert!(offset_of!(KernelArgs, loader_handoff_counter) == 224);
    assert!(offset_of!(KernelArgs, root_read_ticks) == 232);
    assert!(offset_of!(KernelArgs, loader_seed) == 240);
    assert!(offset_of!(KernelArgs, loader_seed_len) == 272);
    assert!(offset_of!(KernelArgs, wall_clock_secs) == 280);
    assert!(offset_of!(KernelArgs, wall_clock_counter) == 288);
    assert!(offset_of!(KernelArgs, wall_clock_known) == 296);
    assert!(offset_of!(KernelArgs, gop_edid) == 304);
    assert!(offset_of!(KernelArgs, gop_edid_len) == 432);
    assert!(offset_of!(KernelArgs, microcode_addr) == 440);
    assert!(offset_of!(KernelArgs, microcode_len) == 448);
    assert!(size_of::<KernelArgs>() == 456);
    assert!(LAYOUT as i32 > 1440 || (LAYOUT as i32) < -1440);
    assert!(align_of::<KernelArgs>() == 8);
};

#[repr(C)]
#[derive(Debug)]
pub struct MemoryMapEntry {
    pub uefi_type: u32,
    pub start: u64,
    pub end: u64,
}
#[cfg(test)]
mod tests {
    use super::*;

    /// Every token is one reading's or the other's, and neither sees the
    /// other's: a `root=` reaching the actuator table panics a kernel.
    #[test]
    fn the_two_readings_of_a_boot_parameter_partition_it() {
        const ROOT: &str = "0123456789abcdef0123456789abcdef";
        const SHIPPING: &str = "root=0123456789abcdef0123456789abcdef";
        const ARMED: &str =
            "root=0123456789abcdef0123456789abcdef,usb-flush-fails,fat-boot-reads-fail";

        assert_eq!(root_uuid(SHIPPING), Some(ROOT));
        assert_eq!(actuators(SHIPPING).count(), 0);

        assert_eq!(root_uuid(ARMED), Some(ROOT));
        assert!(actuators(ARMED).eq(["usb-flush-fails", "fat-boot-reads-fail"]));

        // No root filesystem, and no parameter at all: neither reading invents.
        assert_eq!(root_uuid("usb-flush-fails"), None);
        assert!(actuators("usb-flush-fails").eq(["usb-flush-fails"]));
        assert_eq!(root_uuid(""), None);
        assert_eq!(actuators("").count(), 0);
    }
}
