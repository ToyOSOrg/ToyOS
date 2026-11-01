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
    /// read out of `\toyos\cmdline` on the slot's volume, with the words the
    /// loader appends ([`SLOT_PARAM`] among them), read by [`tokens`].
    ///
    /// Every token is a parameter the kernel claims (`kernel/src/params.rs`)
    /// or an actuator its table declares, and a kernel that carries no
    /// actuators refuses any other rather than ignoring it
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
    /// ROOT's bytes, opaque to the kernel: the slot's ROOT partition read
    /// through the firmware's block I/O. [`Self::supervisor_image`] says what
    /// every [`BootImage`] is.
    pub root_image: BootImage,
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
    /// The unique GUID of the booted slot's volume, the partition the loader
    /// read the kernel and its parameter from, raw as in the GPT entry like
    /// [`Self::root_partition_guid`]; zero with no image.
    pub slot_volume_guid: [u8; 16],
    /// The supervisor's executable and the file server's, which the kernel
    /// hands the supervisor with [`Self::root_image`]: the programs that serve
    /// ROOT, so the kernel opens no file to start the machine.
    ///
    /// **Every `BootImage` is a section of the slot's signed header**, read
    /// into page-aligned `LoaderData` pages and held to its hash before the
    /// loader exits boot services; nothing writes it after. The kernel keeps
    /// each out of its allocator for the machine's life, and refuses by name a
    /// boot whose image is empty, unaligned, outside one `LoaderData`
    /// descriptor or overlapping another region the loader handed.
    pub supervisor_image: BootImage,
    pub fileserver_image: BootImage,
    /// Where firmware loaded this boot from, as the loader read it before
    /// boot services ended. The kernel keeps it unchanged and answers it
    /// through [`crate::syscall::SYS_BOOT_SOURCES`]; it reads no topology or
    /// partition out of it.
    pub boot_sources: BootSources,
}

crate::user_safe! {
    /// The devices this boot's bytes came off. **Provenance, not authority**:
    /// they say where the loader read, never that a device found there is the
    /// one it read or may be trusted; what was read is held to the slot's
    /// signature regardless.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct BootSources {
        /// The partition firmware loaded the loader from, the ESP
        /// ([`crate::inventory::Role::Boot`]). ROOT is read off this
        /// partition's disk.
        pub loader: BootSource,
        /// The booted slot's volume ([`crate::inventory::Role::Slot`]): the
        /// signed header, the kernel, its parameter and the two programs.
        pub slot: BootSource,
    }
}

crate::user_safe! {
    /// One device a boot read: firmware's device path for its handle, and the
    /// PCI function the path runs through last.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct BootSource {
        /// [`Self::path_len`] bytes of firmware's device path (UEFI 2.10 §10),
        /// End node included, and zeroes after; none where firmware named no
        /// path or one the loader refused as malformed or longer than this.
        pub path: [u8; DEVICE_PATH_BYTES],
        pub path_len: u16,
        /// The function whose PCI I/O handle's path is the longest proper
        /// prefix of [`Self::path`], where `GetLocation()` (§14.4.13) placed
        /// it: the PCI hops of a path name no bus, and firmware numbered the
        /// buses the kernel enumerates.
        pub controller: Controller,
        pub reserved: [u8; 6],
    }
}

/// The most bytes of a [`BootSource`]'s path.
pub const DEVICE_PATH_BYTES: usize = 512;

crate::user_safe! {
    /// [`BootSource::controller`]: `present` is 1 where the other fields name
    /// a function, and 0 where they are zero and name none.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct Controller {
        pub present: u8,
        pub bus: u8,
        pub dev: u8,
        pub func: u8,
        pub segment: u16,
        pub reserved: [u8; 2],
    }
}

impl BootSources {
    pub const NONE: Self = Self { loader: BootSource::NONE, slot: BootSource::NONE };
}

impl BootSource {
    pub const NONE: Self =
        Self { path: [0; DEVICE_PATH_BYTES], path_len: 0, controller: Controller::NONE, reserved: [0; 6] };

    /// The path's bytes, or `None` where there is none.
    pub fn path(&self) -> Result<Option<&[u8]>, Unwritten> {
        let len = usize::from(self.path_len);
        match self.path.get(..len) {
            None => Err(Unwritten::PathLen(self.path_len)),
            Some(path) => Ok((len > 0).then_some(path)),
        }
    }
}

impl Controller {
    pub const NONE: Self = Self { present: 0, bus: 0, dev: 0, func: 0, segment: 0, reserved: [0; 2] };

    pub fn at(&self) -> Result<Option<crate::inventory::PciAddr>, Unwritten> {
        match self.present {
            0 => Ok(None),
            1 => Ok(Some(crate::inventory::PciAddr { segment: self.segment, bus: self.bus, dev: self.dev, func: self.func })),
            other => Err(Unwritten::Present(other)),
        }
    }
}

/// A [`BootSource`] field holding what no loader writes: the kernel refuses
/// such a handoff, and a reader refuses such an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unwritten {
    /// A path length past [`DEVICE_PATH_BYTES`].
    PathLen(u16),
    /// A controller's `present` neither 0 nor 1.
    Present(u8),
}

impl core::fmt::Display for Unwritten {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::PathLen(len) => write!(f, "its path is {len} bytes, past {DEVICE_PATH_BYTES}"),
            Self::Present(present) => write!(f, "its controller's present is {present}"),
        }
    }
}

/// One payload the loader hands the kernel: `len` bytes at the physical
/// address `addr`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BootImage {
    pub addr: u64,
    pub len: u64,
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

/// The word the loader appends naming the slot it booted, `A` or `B`: the
/// signed boot parameter is the slot's own, and which slot carried it is a
/// fact only the loader has.
pub const SLOT_PARAM: &str = "boot-slot=";

/// The word the loader appends when it booted the other slot than the one the
/// slot table marks: the marked slot and why it was refused, `B:died`. The
/// kernel logs it, which is how a dead or refused boot reaches the next boot's
/// log.
pub const SLOT_REFUSED_PARAM: &str = "slot-refused=";

/// Every token of the boot parameter: each one a parameter of the kernel's (`params`) or an actuator its table declares.
pub fn tokens(cmdline: &str) -> impl Iterator<Item = &str> {
    cmdline.split(',').filter(|t| !t.is_empty())
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
    assert!(offset_of!(KernelArgs, root_image) == 184);
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
    assert!(offset_of!(KernelArgs, slot_volume_guid) == 456);
    assert!(offset_of!(KernelArgs, supervisor_image) == 472);
    assert!(offset_of!(KernelArgs, fileserver_image) == 488);
    assert!(offset_of!(KernelArgs, boot_sources) == 504);
    assert!(size_of::<KernelArgs>() == 1560);
    assert!(size_of::<BootSource>() == 528);
    assert!(size_of::<BootImage>() == 16);
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

    const ZEROED: KernelArgs = KernelArgs {
        memory_map_addr: 0,
        memory_map_size: 0,
        kernel_memory_addr: 0,
        kernel_memory_size: 0,
        kernel_stack_addr: 0,
        kernel_stack_size: 0,
        rsdp_addr: 0,
        kernel_elf_addr: 0,
        kernel_elf_size: 0,
        gop_framebuffer: 0,
        gop_framebuffer_size: 0,
        gop_width: 0,
        gop_height: 0,
        gop_stride: 0,
        gop_pixel_format: 0,
        boot_pml4_addr: 0,
        boot_partition_start_lba: 0,
        boot_partition_blocks: 0,
        boot_partition_guid: [0; 16],
        boot_partition_present: 0,
        log_partition_guid: [0; 16],
        layout: LAYOUT,
        cmdline_addr: 0,
        cmdline_len: 0,
        root_image: BootImage { addr: 0, len: 0 },
        root_partition_guid: [0; 16],
        loader_entry_counter: 0,
        loader_handoff_counter: 0,
        root_read_ticks: 0,
        loader_seed: [0; SEED_LEN],
        loader_seed_len: 0,
        wall_clock_secs: 0,
        wall_clock_counter: 0,
        wall_clock_known: 0,
        gop_edid: [0; EDID_BLOCK],
        gop_edid_len: 0,
        microcode_addr: 0,
        microcode_len: 0,
        slot_volume_guid: [0; 16],
        supervisor_image: BootImage { addr: 0, len: 0 },
        fileserver_image: BootImage { addr: 0, len: 0 },
        boot_sources: BootSources::NONE,
    };

    #[test]
    fn a_boot_source_names_a_path_and_a_place_only_where_it_carries_them() {
        assert_eq!(ZEROED.boot_sources.loader.path(), Ok(None));
        assert_eq!(ZEROED.boot_sources.slot.controller.at(), Ok(None));
        let mut source = BootSource { path_len: 4, ..BootSource::NONE };
        source.path[..4].copy_from_slice(&[0x7f, 0xff, 4, 0]);
        source.controller = Controller { present: 1, bus: 3, dev: 0, func: 4, ..Controller::NONE };
        assert_eq!(source.path(), Ok(Some(&[0x7f, 0xff, 4, 0][..])));
        assert_eq!(source.controller.at(), Ok(Some(crate::inventory::PciAddr { segment: 0, bus: 3, dev: 0, func: 4 })));
    }

    #[test]
    fn a_field_no_loader_writes_is_refused_and_not_read() {
        let full = BootSource { path_len: DEVICE_PATH_BYTES as u16, ..BootSource::NONE };
        assert_eq!(full.path().map(|p| p.map(<[u8]>::len)), Ok(Some(DEVICE_PATH_BYTES)));
        assert_eq!(BootSource { path_len: 513, ..BootSource::NONE }.path(), Err(Unwritten::PathLen(513)));
        assert_eq!(BootSource { path_len: u16::MAX, ..BootSource::NONE }.path(), Err(Unwritten::PathLen(u16::MAX)));
        assert_eq!(Controller { present: 2, ..Controller::NONE }.at(), Err(Unwritten::Present(2)));
    }
}
