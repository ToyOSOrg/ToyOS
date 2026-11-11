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
    pub kernel_image_addr: u64,
    pub kernel_image_size: u64,
    pub gop_framebuffer: u64,
    pub gop_framebuffer_size: u64,
    pub gop_width: u32,
    pub gop_height: u32,
    pub gop_stride: u32,
    pub gop_pixel_format: u32,
    /// Maps the low physical memory both at identity and at the high half.
    pub boot_pml4_addr: u64,
    /// The layout the loader wrote this struct in: [`LAYOUT`] from a loader built
    /// with this file. The kernel refuses any other value before it reads a
    /// field after this one, so every field before it keeps its offset in every
    /// layout.
    pub layout: u32,
    /// The boot parameter, as ASCII with no terminator: comma-separated tokens
    /// read out of `\toyos\cmdline` on the slot's volume, with the word the
    /// loader appends ([`SLOT_PARAM`]), read by [`tokens`].
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
    /// The supervisor's executable and the file server's, which the kernel
    /// hands the supervisor with [`Self::root_image`]: the programs that serve
    /// ROOT, so the kernel opens no file to start the machine.
    ///
    /// **Every `BootImage` is page-aligned `LoaderData` pages nothing writes
    /// once the loader exits boot services**, and these two and ROOT are
    /// sections of the slot's signed header, held to their hashes before it
    /// does. The kernel keeps each out of its allocator for the machine's
    /// life, and refuses by name a boot whose image is empty, unaligned,
    /// outside one `LoaderData` descriptor or overlapping another region the
    /// loader handed.
    pub supervisor_image: BootImage,
    pub fileserver_image: BootImage,
    /// What the loader names of where it booted from, written by it for the
    /// supervisor: bytes the kernel hands on and never reads.
    pub boot_names: BootImage,
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
/// size folded in. A loader older than the word's place wrote the low half of
/// its partition's first block at this offset, which no ESP starts at.
pub const LAYOUT: u32 = 0x5459_0000 | core::mem::size_of::<KernelArgs>() as u32;

/// The boot parameter on which the loader writes 0 as [`KernelArgs::layout`]:
/// the negative control on the kernel's refusal, and read by both of them.
pub const WRITE_NO_LAYOUT_PARAM: &str = "loader-writes-no-layout";

/// The word the loader appends naming the slot it booted, `A` or `B`: the
/// signed boot parameter is the slot's own, and which slot carried it is a
/// fact only the loader has.
pub const SLOT_PARAM: &str = "boot-slot=";

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
    assert!(offset_of!(KernelArgs, layout) == 112);
    assert!(offset_of!(KernelArgs, cmdline_addr) == 120);
    assert!(offset_of!(KernelArgs, cmdline_len) == 128);
    assert!(offset_of!(KernelArgs, root_image) == 136);
    assert!(offset_of!(KernelArgs, loader_entry_counter) == 152);
    assert!(offset_of!(KernelArgs, loader_handoff_counter) == 160);
    assert!(offset_of!(KernelArgs, root_read_ticks) == 168);
    assert!(offset_of!(KernelArgs, loader_seed) == 176);
    assert!(offset_of!(KernelArgs, loader_seed_len) == 208);
    assert!(offset_of!(KernelArgs, wall_clock_secs) == 216);
    assert!(offset_of!(KernelArgs, wall_clock_counter) == 224);
    assert!(offset_of!(KernelArgs, wall_clock_known) == 232);
    assert!(offset_of!(KernelArgs, gop_edid) == 240);
    assert!(offset_of!(KernelArgs, gop_edid_len) == 368);
    assert!(offset_of!(KernelArgs, microcode_addr) == 376);
    assert!(offset_of!(KernelArgs, microcode_len) == 384);
    assert!(offset_of!(KernelArgs, supervisor_image) == 392);
    assert!(offset_of!(KernelArgs, fileserver_image) == 408);
    assert!(offset_of!(KernelArgs, boot_names) == 424);
    assert!(size_of::<KernelArgs>() == 440);
    assert!(size_of::<BootImage>() == 16);
    assert!(align_of::<KernelArgs>() == 8);
};

#[repr(C)]
#[derive(Debug)]
pub struct MemoryMapEntry {
    pub uefi_type: u32,
    pub start: u64,
    pub end: u64,
}
