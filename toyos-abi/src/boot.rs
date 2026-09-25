#[repr(C)]
#[derive(Debug, Clone, Copy)]
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
    /// Minutes to add to the CMOS RTC's own reading to get UTC, as firmware
    /// reported it in `EFI_TIME::TimeZone`.
    ///
    /// The RTC's registers carry a wall clock and no zone, and no two operating
    /// systems agree on which zone that is: a machine that has ever run Windows
    /// keeps local time there, one that has only run Linux keeps UTC. Firmware
    /// is the one party that both knows and can be asked, and `GetTime` is the
    /// call — a *runtime* service, so it is asked here rather than in the
    /// kernel, which never maps the runtime.
    ///
    /// UEFI's relation is `Localtime = UTC - TimeZone`, so a machine keeping
    /// local time in UTC+2 reports -120 and the kernel adds -120 minutes to what
    /// it reads off the CMOS.
    pub rtc_utc_offset_minutes: i32,
    /// Whether firmware answered the question above at all.
    ///
    /// Zero when `GetTime` failed, or reported `EFI_UNSPECIFIED_TIMEZONE`, or
    /// named an offset outside the range its own spec gives the field. The
    /// middle one is the ordinary state of a machine nothing has ever told its
    /// zone to, and it is what OVMF ships. The kernel then treats the RTC as UTC
    /// and says so, because with the one party that knows declining to answer
    /// there is nothing else left to assume.
    ///
    /// A flag rather than a sentinel in the field above, for the same reason
    /// [`Self::boot_partition_present`] is one: `0x7FF` is a value the *wire*
    /// format defines, and carrying it inward would make every reader of this
    /// struct know that.
    pub rtc_utc_offset_known: u32,
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
    /// How many of [`Self::root_bridge_windows`] firmware named.
    pub root_bridge_window_count: u64,
    /// The memory windows the platform's root bridges decode, as the addresses
    /// a CPU issues.
    ///
    /// **An address outside every one of these is not free space, it is
    /// unrouted**: a read of it answers all-ones, which is what an absent
    /// device answers too. So this is what a BAR the kernel re-places has to
    /// land inside, and there is no deriving it from the memory map — the map
    /// says what firmware *used*, not what the bridge would decode.
    pub root_bridge_windows: [RootBridgeWindow; MAX_ROOT_BRIDGE_WINDOWS],
    /// Where the loader put ROOT: the whole partition, read through the
    /// firmware's block I/O into pages of [`ROOT_IMAGE_MEMORY_TYPE`], so the
    /// kernel mounts it from memory and never frees it.
    ///
    /// These are the bytes a signature check over ROOT has to cover: the loader
    /// writes them once, and nothing writes them between that read and the
    /// kernel's mount. Zero length is a loader that handed no image, and the
    /// kernel refuses to boot on it by name; it never reads ROOT anywhere else.
    pub root_image_addr: u64,
    pub root_image_len: u64,
}

/// The UEFI memory type the loader allocates ROOT's image as: one of the
/// values UEFI 2.11 §7.2.1 leaves to an OS loader (`0x8000_0000..`), so the
/// firmware's map marks those pages as nobody's free memory and the kernel's
/// allocator, which takes only the types it knows are free, never hands them out.
pub const ROOT_IMAGE_MEMORY_TYPE: u32 = 0x8000_7201;

/// The boot parameter on which the loader hands the kernel no ROOT image: the
/// negative control on the kernel's refusal, and read by both of them.
pub const WITHHOLD_ROOT_PARAM: &str = "loader-withholds-root";

/// The most windows the loader will carry.
pub const MAX_ROOT_BRIDGE_WINDOWS: usize = 64;

/// One memory window a root bridge decodes.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RootBridgeWindow {
    pub base: u64,
    pub length: u64,
}

impl RootBridgeWindow {
    /// The first address past the window, saturating: a firmware-named length
    /// is untrusted input and an extent that would wrap is one this cannot
    /// contain anything past.
    pub fn end(&self) -> u64 {
        self.base.saturating_add(self.length)
    }

    /// Whether every address of the `length` bytes at `base` is one this window
    /// decodes.
    ///
    /// The whole extent and not its first address: a BAR that starts inside a
    /// window and runs past it decodes addresses the bridge does not. An extent
    /// of no length carries no address for a window to decode and is inside
    /// none of them.
    pub fn holds(&self, base: u64, length: u64) -> bool {
        length != 0
            && base >= self.base
            && base.checked_add(length).is_some_and(|end| end <= self.end())
    }
}

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

impl KernelArgs {
    /// Firmware's answer about the zone the RTC keeps, as one value.
    ///
    /// The two fields exist because this struct is a C layout shared by two
    /// binaries; this is where they become the option they describe, and no
    /// caller inward of here handles the pair.
    pub fn rtc_utc_offset(&self) -> Option<i32> {
        (self.rtc_utc_offset_known != 0).then_some(self.rtc_utc_offset_minutes)
    }

    /// The windows firmware named, and none of the array behind them. The
    /// loader is the only writer of the count, so one past the array panics
    /// here rather than clamping.
    pub fn root_bridge_windows(&self) -> &[RootBridgeWindow] {
        &self.root_bridge_windows[..self.root_bridge_window_count as usize]
    }
}

/// The kernel's `_start` reads three of these fields out of `rdi` by hardcoded
/// byte offset, before Rust code runs and before there is a stack to call a
/// getter on. Adding a field anywhere but the end moves them silently, and the
/// symptom is a stack pointer pointing at nothing.
///
/// The size and alignment are here for the other half of the contract: the
/// bootloader writes this struct and the kernel reads it, and the two are
/// separate binaries built for separate targets. They share this file, so they
/// cannot disagree about the layout — but only as long as nothing else does
/// the arithmetic by hand.
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
    assert!(offset_of!(KernelArgs, rtc_utc_offset_minutes) == 164);
    assert!(offset_of!(KernelArgs, rtc_utc_offset_known) == 168);
    assert!(offset_of!(KernelArgs, cmdline_addr) == 176);
    assert!(offset_of!(KernelArgs, cmdline_len) == 184);
    assert!(offset_of!(KernelArgs, root_bridge_window_count) == 192);
    assert!(offset_of!(KernelArgs, root_bridge_windows) == 200);
    assert!(offset_of!(KernelArgs, root_image_addr) == 1224);
    assert!(offset_of!(KernelArgs, root_image_len) == 1232);
    assert!(size_of::<KernelArgs>() == 1240);
    assert!(align_of::<KernelArgs>() == 8);
    assert!(size_of::<RootBridgeWindow>() == 16);
    assert!(align_of::<RootBridgeWindow>() == 8);
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

    #[test]
    fn a_window_holds_an_extent_and_not_merely_its_first_address() {
        let window = RootBridgeWindow { base: 0xa080_0000, length: 0x1f80_0000 };
        assert_eq!(window.end(), 0xc000_0000);

        assert!(window.holds(0xbcf0_0000, 0x2_0000));
        assert!(window.holds(0xa080_0000, 0x2_0000));
        assert!(window.holds(0xa080_0000, 0x1f80_0000));
        assert!(window.holds(0xbfff_f000, 0x1000));
        assert!(!window.holds(0xa07f_f000, 0x2_0000));
        assert!(!window.holds(0x9920_0000, 0x1000));
        assert!(!window.holds(0xbfff_f000, 0x1001));
        assert!(!window.holds(0xbfff_0000, 0x1001_0000));
        assert!(!window.holds(0xc000_0000, 0));
        assert!(!window.holds(0xa080_0000, 0));

        let whole = RootBridgeWindow { base: 1, length: u64::MAX };
        assert_eq!(whole.end(), u64::MAX);
        assert!(!whole.holds(2, u64::MAX));
    }

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
        rtc_utc_offset_minutes: 0,
        rtc_utc_offset_known: 0,
        cmdline_addr: 0,
        cmdline_len: 0,
        root_bridge_window_count: 0,
        root_bridge_windows: [RootBridgeWindow { base: 0, length: 0 };
            MAX_ROOT_BRIDGE_WINDOWS],
        root_image_addr: 0,
        root_image_len: 0,
    };

    #[test]
    fn the_kernel_is_handed_the_windows_firmware_named_and_none_of_the_array_behind_them() {
        assert!(ZEROED.root_bridge_windows().is_empty());

        let low = RootBridgeWindow { base: 0xa080_0000, length: 0x1f80_0000 };
        let high = RootBridgeWindow { base: 0x40_0000_0000, length: 0x20_0000_0000 };
        let behind = RootBridgeWindow { base: 0xdead_0000, length: 0x1000 };
        let mut args = KernelArgs { root_bridge_window_count: 2, ..ZEROED };
        args.root_bridge_windows[0] = low;
        args.root_bridge_windows[1] = high;
        args.root_bridge_windows[2] = behind;

        assert_eq!(args.root_bridge_windows(), &[low, high][..]);
    }

    #[test]
    #[should_panic(expected = "out of range")]
    fn a_count_past_the_array_is_refused_rather_than_clamped() {
        let args =
            KernelArgs { root_bridge_window_count: MAX_ROOT_BRIDGE_WINDOWS as u64 + 1, ..ZEROED };
        let _windows = args.root_bridge_windows();
    }
}
