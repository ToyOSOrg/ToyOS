#![no_main]
#![no_std]

extern crate alloc;

use core::mem;

use alloc::vec;
use alloc::alloc::Layout;
use toyos_elf::section::SectionTable;
use toyos_elf::{rela, ImageOffset, Op, RelaTable, SymTab};
use uefi::{
    prelude::*,
    CStr16,
    proto::console::gop::{GraphicsOutput, PixelFormat},
    proto::device_path::{media::{PartitionFormat, PartitionSignature}, DevicePath, DevicePathNode, DeviceType, DeviceSubType},
    proto::loaded_image::LoadedImage,
    proto::media::file::{File, FileAttribute, FileInfo, FileMode},
    table::{boot::{MemoryAttribute, MemoryType, PAGE_SIZE}, cfg::ACPI2_GUID, runtime::ResetType},
    Event,
};
use toyos_abi::boot::{KernelArgs, MemoryMapEntry, RootBridgeWindow, MAX_ROOT_BRIDGE_WINDOWS};
use toyos_bootmap::{Plan, BOOT_MAP_BYTES, MAX_PAGES, ROOT_HIGH_HALF, ROOT_IDENTITY};
use toyos_update::policy;
use toyos_update::record::{self, Booted, Ended, Record};

/// Every line this loader prints: the firmware's console, and the file on the
/// stick once [`loaderlog::open`] has one, under one [`stamp`]. The arguments
/// are evaluated once, so a line that reads a clock says the same on both.
macro_rules! println {
    ($($arg:tt)*) => {
        $crate::stamp::say(core::format_args!($($arg)*))
    };
}

mod arch;
mod attempt;
mod blackbox;
mod bootnext;
mod protocol;
mod floor;
mod gcd;
mod loaderlog;
mod rootbridge;
mod rootimage;
mod seed;
mod slot;
mod stamp;
mod wallclock;
mod watchdog;

/// The largest file the bootloader will read off the ESP.
const MAX_ESP_FILE: u64 = 1024 * 1024 * 1024;

/// Descriptors of room held above what the map measured, for the descriptors
/// the two allocations between that measurement and `ExitBootServices` add: the
/// vector below, and the buffer `exit_boot_services` takes the map into.
///
/// **The margin is not what makes the loop safe** — the loop refuses to grow
/// the vector at all, and this only decides how much of a real map is kept.
/// Each allocation splits at most one free region in two, so four would do;
/// this is beyond any plausible firmware and costs 1.5 KiB.
const MAP_MARGIN: usize = 64;

fn alloc_kernel_memory(size: usize) -> vec::Vec<u8> {
    const KERNEL_ALIGN: usize = 2 * 1024 * 1024; // 2MB
    let layout = Layout::from_size_align(size, KERNEL_ALIGN).expect("invalid layout");
    // SAFETY: `layout` has non-zero size so `alloc_zeroed`'s "layout must have
    // non-zero size" precondition always holds.
    let ptr = unsafe { alloc::alloc::alloc_zeroed(layout) };
    assert!(!ptr.is_null(), "kernel allocation failed");
    // SAFETY: `ptr` was just returned by the global allocator for exactly
    // `layout`, so it is non-null (asserted above), currently allocated, and
    // sized and aligned for `size` bytes. `len == capacity == size` is the
    // allocation's own size, not a separate claim.
    unsafe { vec::Vec::from_raw_parts(ptr, size, size) }
}

struct LoadedKernel {
    pub memory: vec::Vec<u8>,
    pub entry_offset: usize,
    pub stack_offset: usize,
    pub stack_size: usize,
}

fn load_file_bytes(handle: Handle, system_table: &SystemTable<Boot>, path: &CStr16) -> vec::Vec<u8> {
    let mut fs = system_table
        .boot_services()
        .get_image_file_system(handle)
        .expect("Failed to get file system");

    let mut file = fs
        .open_volume()
        .expect("Failed to open volume")
        .open(path, FileMode::Read, FileAttribute::default())
        .expect("Failed to open file")
        .into_regular_file()
        .expect("Failed to convert to regular file");

    let file_info_len = file
        .get_info::<FileInfo>(&mut [])
        .expect_err("Failed to get file info len")
        .data()
        .expect("File info len was None");

    let mut buffer = vec![0; file_info_len];
    let file_info = file
        .get_info::<FileInfo>(&mut buffer)
        .expect("Failed to get file info");

    let declared = file_info.file_size();
    assert!(
        declared <= MAX_ESP_FILE,
        "the ESP reports a {declared}-byte file, past the {MAX_ESP_FILE}-byte bound"
    );
    let size = declared as usize;
    let mut bytes = alloc_uninit(size);
    let read = file.read(&mut bytes).expect("Failed to read file");
    // Every byte handed back must have come from the file: the buffer was never
    // zeroed, so a short read would leave allocator garbage in the tail and the
    // caller would parse it as image content.
    assert_eq!(read, size, "short read: {read} of {size} bytes");

    bytes
}

/// A buffer to be filled by a read, allocated *without* zeroing it first.
/// The caller must check that the read filled the whole buffer.
///
/// Do not simplify to `vec![0; size]`: that memsets the whole file
/// immediately before `File::read` overwrites every byte. The chain is not
/// visible at the call site — `vec![0u8; n]` takes `SpecFromElem`'s zero branch
/// to `RawVec::with_capacity_zeroed_in` and so to `alloc_zeroed`, and uefi
/// 0.26's allocator implements only `alloc`/`dealloc`, so it falls through to
/// `GlobalAlloc`'s default of `alloc` plus `write_bytes(ptr, 0, size)`.
fn alloc_uninit(size: usize) -> vec::Vec<u8> {
    // `\toyos\cmdline` is legitimately empty on a machine with no boot
    // arguments, so `size` reaches here as 0 in real boots, not just in
    // theory. `alloc`'s "layout must have non-zero size" precondition would
    // not hold for it, and there is nothing to allocate anyway.
    if size == 0 {
        return vec::Vec::new();
    }
    let layout = Layout::from_size_align(size, 1).expect("invalid layout");
    // SAFETY: `layout` has non-zero size, guaranteed by the early return above.
    let ptr = unsafe { alloc::alloc::alloc(layout) };
    assert!(!ptr.is_null(), "file buffer allocation failed ({size} bytes)");
    // SAFETY: `ptr` was just returned by the global allocator for exactly
    // `layout`, so it is non-null (asserted above), currently allocated, and
    // sized and aligned for `size` bytes. `len == capacity == size` is the
    // allocation's own size, not a separate claim.
    unsafe { vec::Vec::from_raw_parts(ptr, size, size) }
}

/// Which partition this image was loaded from, as firmware knows it.
struct BootPartition {
    guid: [u8; 16],
    start_lba: u64,
    blocks: u64,
}

/// Ask firmware which partition it loaded us from.
///
/// `LoadedImage->DeviceHandle` is the handle the image came off, and the
/// HARDDRIVE node of that handle's device path carries the partition's
/// **unique** GUID — a name for one partition on one disk, which is what the
/// kernel needs and what neither a type GUID nor a disk GUID can give it. It
/// has to be read here, because the device path protocol dies with Boot
/// Services and there is no way to ask afterwards.
///
/// `None` is a machine, not a failure: PXE, an unpartitioned device, and a
/// signature type firmware chose not to fill in all land here, and the kernel
/// is expected to boot on all of them knowing it has no partition of its own.
/// Every early-return below is one of those, so none of them panics.
fn boot_partition(handle: Handle, system_table: &SystemTable<Boot>) -> Option<BootPartition> {
    let bs = system_table.boot_services();
    let image = protocol::exclusive::<LoadedImage>(bs, handle).ok()?;
    let device = image.device()?;
    let path = protocol::exclusive::<DevicePath>(bs, device).ok()?;

    let is_hard_drive = |node: &&DevicePathNode| {
        node.full_type() == (DeviceType::MEDIA, DeviceSubType::MEDIA_HARD_DRIVE)
    };
    // Exactly one, not the last one. A path with two HARDDRIVE nodes describes
    // a partition inside a partition, and picking either is guessing which of
    // the two the kernel's block device will be looking at.
    let mut nodes = path.node_iter().filter(is_hard_drive);
    let node = nodes.next()?;
    if nodes.next().is_some() {
        println!("Boot partition: the device path has more than one HARDDRIVE node, so it is ignored");
        return None;
    }

    let hd: &uefi::proto::device_path::media::HardDrive = node.try_into().ok()?;
    if hd.partition_format() != PartitionFormat::GPT {
        println!("Boot partition: firmware says this is not a GPT partition, so it is ignored");
        return None;
    }
    let PartitionSignature::Guid(guid) = hd.partition_signature() else {
        println!("Boot partition: firmware named it with no GUID signature, so it is ignored");
        return None;
    };
    Some(BootPartition {
        guid: guid.to_bytes(),
        start_lba: hd.partition_start(),
        blocks: hd.partition_size(),
    })
}

/// Name the partition the kernel's log goes on, without reading it.
///
/// Written beside this loader by `src/image.rs`, which draws the GUID and
/// stamps the same sixteen bytes into the GPT entry. Read here because this is
/// the volume firmware designated and because the kernel has no filesystem yet:
/// the identity is *given* all the way down, and nothing at any level scans for
/// a partition of the right type or format.
///
/// A missing or short file panics, like every other check in this file. The
/// same function writes the loader and this file, so a volume with one of them
/// was assembled by something that is not this project — and booting it anyway
/// would mean a kernel that quietly has nowhere to write its log, on the
/// machine that has no other channel.
fn log_partition_guid(handle: Handle, system_table: &SystemTable<Boot>) -> [u8; 16] {
    let bytes = load_file_bytes(handle, system_table, cstr16!("\\toyos\\log.guid"));
    <[u8; 16]>::try_from(bytes.as_slice()).unwrap_or_else(|_| {
        panic!("\\toyos\\log.guid holds {} bytes, wanted 16", bytes.len())
    })
}

/// [`toyos_tco::FIRMWARE_BOUND_MS`] in the seconds `set_watchdog_timer` takes.
const FIRMWARE_WATCHDOG_SECS: usize = (toyos_tco::FIRMWARE_BOUND_MS / 1_000) as usize;

/// The head of every line about the attempt count this image has on its stick.
const ATTEMPTS: &str = "Boot attempts:";

/// **What a boot that never reported looks like from the next one**, and the
/// line the T14 driver reads as its own verdict.
///
/// Written where this pass refuses to boot a kernel because the last one was
/// handed the machine and said nothing back. Held to the host's spelling by
/// `toyos_build::bootlog`'s own gate.
const HUNG_WITHOUT_A_RECORD: &str =
    "Boot attempts: the previous boot of this image never reported; the machine is handed back";

/// What firmware logs if that countdown expires. Codes to `0xffff` are reserved
/// for firmware's own use and this is the first one an application may take;
/// `uefi`'s `set_watchdog_timer` refuses a reserved one outright.
const WATCHDOG_CODE: u64 = 0x0001_0000;

/// Kernel virtual base: all physical memory is mapped here in the kernel's address space.
const PHYS_OFFSET: u64 = 0xFFFF_8000_0000_0000;

/// `[offset, offset + len)` of the file, or `None` when that is not wholly
/// inside it.
///
/// Every `offset` and `len` passed here came out of the image's own headers, so
/// both are numbers the file chose: the addition is checked and the bytes are
/// taken with `get` rather than indexed. Each caller refuses on `None` — a
/// table this cannot cover is never read short.
fn file_range(bytes: &[u8], offset: u64, len: u64) -> Option<&[u8]> {
    let start = usize::try_from(offset).ok()?;
    let end = usize::try_from(offset.checked_add(len)?).ok()?;
    bytes.get(start..end)
}

fn load_kernel_elf(kernel_elf_bytes: &[u8]) -> LoadedKernel {
    let layout = toyos_elf::Layout::parse(kernel_elf_bytes, arch::ELF_MACHINE)
        .unwrap_or_else(|e| panic!("kernel.elf: {e}"));

    // Section headers are optional to `toyos-elf`, which loads programs whose
    // sections carry only symbol names. Here they carry the relocations that
    // make the image runnable, so a file with no readable table is refused
    // rather than started unrelocated.
    let sections = layout
        .section_headers()
        .and_then(|table| file_range(kernel_elf_bytes, table.file_offset, table.byte_len() as u64))
        .map(SectionTable::new)
        .expect("kernel.elf: no section header table inside the file");

    let stack_size: usize = 8 * 1024 * 1024; // 8MB

    println!("Kernel stack size: {}", stack_size);
    // The extent's end is the largest `p_vaddr + p_memsz` over the `PT_LOAD`
    // segments, and the image is laid out at its own vaddrs — so it is what the
    // kernel's memory has to cover before the stack is added after it.
    let extent = layout.extent();
    let placed = toyos_elf::StackedImage::place(extent.max(), stack_size as u64)
        .expect("kernel.elf: image plus stack does not fit an allocation");
    let mem_size = usize::try_from(placed.size).expect("kernel.elf: image plus stack does not fit an allocation");
    // Where an image offset lies in `process_mem`: the image sits at its own
    // vaddrs, which begin at the extent's start.
    let at = |offset: ImageOffset| (extent.min() + offset.get()) as usize;

    println!("Kernel memory size: {}", mem_size);

    let mut process_mem = alloc_kernel_memory(mem_size);
    println!("Kernel memory located at: {:?}", process_mem.as_ptr());

    for segment in layout.segments() {
        println!("Loading segment: {:?}", segment);
        let src = file_range(kernel_elf_bytes, segment.file_offset(), segment.filesz())
            .expect("kernel.elf: PT_LOAD file extent is past the end of the file");
        let vstart = at(segment.image().start());
        // In bounds by construction: `mem_size` is at least
        // `p_vaddr + p_memsz` for this segment and `p_filesz <= p_memsz`.
        process_mem[vstart..vstart + src.len()].copy_from_slice(src);
    }

    let rela_sections =
        sections.rela_sections().unwrap_or_else(|form| panic!("kernel.elf: {form} is not supported"));

    // Both fields of a relocation index the image and both come out of the
    // file: `r_offset` is the destination of an 8-byte store and `r_addend` the
    // address stored. Unchecked, the store is an arbitrary write anywhere in the
    // machine, made before ExitBootServices with firmware still live — so every
    // entry goes through the parse the kernel's own loader uses.
    let rules = rela::Rules {
        extent,
        window: (0, mem_size as u64),
        tls: None,
    };
    let mut reloc_count = 0u64;
    for section in rela_sections {
        let table = file_range(kernel_elf_bytes, section.offset, section.size)
            .expect("kernel.elf: SHT_RELA section is past the end of the file");
        for raw in RelaTable::new(table, arch::ELF_MACHINE).iter() {
            let parsed = rela::parse(raw, &rules, SymTab::empty()).unwrap_or_else(|e| panic!("kernel.elf: {e}"));
            let Some((offset, Op::Relative(target))) = parsed.map(|r| (r.offset(), r.op())) else {
                panic!("kernel.elf: unsupported relocation type {:?}", raw.kind());
            };
            // SAFETY: `target` is inside the image, so this is at most one byte
            // past the end of `process_mem`'s allocation — in bounds for
            // pointer arithmetic, and never dereferenced: only the resulting
            // address is used.
            let value = PHYS_OFFSET + unsafe { process_mem.as_ptr().add(at(target)) } as u64;
            unsafe {
                // SAFETY: the parse put `offset + 8` inside `[0, mem_size)`, so
                // the 8-byte write lands fully inside `process_mem`'s
                // allocation. `write_unaligned`, not `write`: an `r_offset`
                // from the file is not guaranteed 8-byte aligned by anything
                // checked here, only by the linker emitting
                // `R_X86_64_RELATIVE` against aligned slots — a fact this
                // reader has no way to verify.
                process_mem
                    .as_mut_ptr()
                    .add(offset as usize)
                    .cast::<u64>()
                    .write_unaligned(value);
            }
            reloc_count += 1;
        }
    }
    println!("Applied {} relocations", reloc_count);

    LoadedKernel {
        memory: process_mem,
        entry_offset: at(layout.entry()),
        stack_offset: placed.stack as usize,
        stack_size,
    }
}

struct GopInfo {
    framebuffer: u64,
    framebuffer_size: u64,
    width: u32,
    height: u32,
    stride: u32,
    pixel_format: u32,
}

/// The mode is the firmware's: `Mode->Info` is the mode it already set for the
/// panel (UEFI 2.11 §12.9.2, "Current Mode of the graphics device"), and
/// §12.9.2.2's `SetMode` is never called.
fn query_gop(system_table: &SystemTable<Boot>) -> Option<GopInfo> {
    let bs = system_table.boot_services();
    let gop_handle = bs.get_handle_for_protocol::<GraphicsOutput>().ok()?;
    let mut gop = protocol::get::<GraphicsOutput>(bs, gop_handle).ok()?;

    let mode = gop.current_mode_info();
    let (width, height) = mode.resolution();
    let stride = mode.stride();
    let pixel_format = match mode.pixel_format() {
        PixelFormat::Rgb => 0,
        PixelFormat::Bgr => 1,
        // UEFI 2.11 §12.9.2: `PixelBltOnly` "does not support a physical frame
        // buffer", so this display has no scanout for the kernel to inherit.
        PixelFormat::BltOnly => {
            println!("GOP: {}x{} is Blt-only, so this display publishes no framebuffer", width, height);
            return None;
        }
        // Refused by name, not swapped: the mode is not this loader's to pick.
        other => panic!(
            "GOP: the firmware's mode is {width}x{height} {other:?}, and the kernel \
             scans out RGB or BGR only"
        ),
    };

    let mut fb = gop.frame_buffer();
    let framebuffer = fb.as_mut_ptr() as u64;
    let framebuffer_size = fb.size() as u64;

    println!("{} {}x{} stride={} format={} fb={:#x} size={}",
        loaderlog::GOP_AT, width, height, stride, pixel_format, framebuffer, framebuffer_size);

    Some(GopInfo {
        framebuffer,
        framebuffer_size,
        width: width as u32,
        height: height as u32,
        stride: stride as u32,
        pixel_format,
    })
}

/// Write `plan` into `pt_mem` and return its root table's physical address.
///
/// # Safety
/// `pt_mem` is [`toyos_bootmap::MAX_PAGES`] pages of zeroed memory, 4096-aligned.
unsafe fn build_boot_page_tables(pt_mem: *mut u8, plan: &Plan) -> u64 {
    use arch::encoding::{block, page, table};
    use toyos_bootmap::Slot;

    let mut next_page = 0usize;
    let mut alloc_page = || -> *mut u64 {
        let page = pt_mem.add(next_page * 4096) as *mut u64;
        next_page += 1;
        page
    };

    let root = alloc_page();
    let identity_pdpt = alloc_page();
    let high_pdpt = alloc_page();
    let mut directories = [core::ptr::null_mut::<u64>(); toyos_bootmap::MAX_DIRECTORIES];
    for (slot, gib) in plan.directories().iter().enumerate() {
        let pd = alloc_page();
        directories[slot] = pd;
        *identity_pdpt.add(*gib as usize) = table(pd as u64);
        *high_pdpt.add(*gib as usize) = table(pd as u64);
    }

    let mut fine = [core::ptr::null_mut::<u64>(); toyos_bootmap::MAX_PAGES];
    for (table_at, (directory, index)) in plan.fine_slots().enumerate() {
        let leaves = alloc_page();
        fine[table_at] = leaves;
        *directories[directory].add(index) = table(leaves as u64);
    }

    for entry in plan.entries() {
        match entry.slot {
            Slot::Directory { directory, index } => {
                *directories[directory].add(index) = block(entry.phys, entry.cache)
            }
            Slot::Fine { table, index } => *fine[table].add(index) = page(entry.phys, entry.cache),
        }
    }

    *root.add(ROOT_IDENTITY) = table(identity_pdpt as u64);
    *root.add(ROOT_HIGH_HALF) = table(high_pdpt as u64);

    root as u64
}

/// Every range firmware's map says is write-back memory, `(base, length)`:
/// a descriptor carrying `EFI_MEMORY_WB` and not one of the two I/O types,
/// which a firmware may give the attribute without meaning memory.
fn write_back_memory(system_table: &SystemTable<Boot>) -> vec::Vec<(u64, u64)> {
    let boot_services = system_table.boot_services();
    let sizes = boot_services.memory_map_size();
    // Room for the descriptors allocating this buffer itself may add.
    let mut buffer = vec![0u8; sizes.map_size + 8 * sizes.entry_size];
    let map = boot_services.memory_map(&mut buffer).expect("the memory map, before the exit");
    map.entries()
        .filter(|d| d.att.contains(MemoryAttribute::WRITE_BACK))
        .filter(|d| d.ty != MemoryType::MMIO && d.ty != MemoryType::MMIO_PORT_SPACE)
        .map(|d| (d.phys_start, d.page_count * PAGE_SIZE as u64))
        .collect()
}

/// Say whether `at .. at + len` is inside the boot map, and refuse the boot
/// where it is not: a pointer the kernel dereferences before `mm::init` and
/// cannot reach loses every parameter of this boot, not one.
///
/// Before `ExitBootServices` only — past it neither the print nor the panic
/// survives — and said before it is asserted, because `assert!` panics through
/// uefi-services, whose handler reaches the console and not `loader.log`.
fn report_reach(what: &str, at: u64, len: u64) {
    let inside = at.checked_add(len).is_some_and(|end| end <= BOOT_MAP_BYTES);
    println!(
        "{what}: {at:#x}+{len:#x} {} the {BOOT_MAP_BYTES:#x}-byte boot map",
        if inside { "is inside" } else { "DOES NOT FIT" },
    );
    assert!(
        inside,
        "{what} at {at:#x}+{len:#x} is outside the boot map, so the kernel would read none of it"
    );
}

#[allow(clippy::too_many_arguments)]
fn start_kernel(kernel: LoadedKernel, kernel_elf_bytes: vec::Vec<u8>, cmdline: vec::Vec<u8>, rsdp_addr: u64, gop: Option<GopInfo>, boot_part: Option<BootPartition>, log_partition_guid: [u8; 16], root_image: rootimage::RootImage, entry_counter: u64, wall_clock: Option<(toyos_wallclock::Civil, u64)>, system_table: SystemTable<Boot>) -> ! {
    // Said before it is refused, for `report_reach`'s reason.
    match arch::cpu_as_entered() {
        Ok(None) => {}
        Ok(Some(line)) => println!("{line}"),
        Err(why) => {
            println!("{why}");
            panic!("{why}");
        }
    }

    // The last of the firmware questions, and asked here for the same reason
    // the GOP's was asked before this: the protocol dies with boot services.
    //
    // Both readers answer memory the root bridges decode, which is what this
    // array carries and the only thing the kernel asks of it.
    let mut root_bridge_windows = [RootBridgeWindow::default(); MAX_ROOT_BRIDGE_WINDOWS];
    let named = rootbridge::windows(&system_table, &mut root_bridge_windows);
    let free = gcd::free_mmio(&system_table, &mut root_bridge_windows[named..]);
    let root_bridge_window_count = (named + free) as u64;

    // Pre-allocated before exiting boot services, and flat: `alloc_page` splits
    // it into 512-entry pages.
    let pt_layout = Layout::from_size_align(MAX_PAGES * 4096, 4096).unwrap();
    // SAFETY: `layout` has non-zero size and its 4096 alignment is what every
    // page-table page below needs — the low 12 bits of an entry are flags, not
    // address bits.
    let pt_mem = unsafe { alloc::alloc::alloc_zeroed(pt_layout) };
    assert!(!pt_mem.is_null(), "page table allocation failed");

    // Where firmware loaded this image, which is where the x86-64 switch to
    // the boot map runs from: the map holds it wherever that is.
    let loader = {
        let bs = system_table.boot_services();
        let image = protocol::exclusive::<LoadedImage>(bs, bs.image_handle())
            .expect("firmware answers LoadedImage for the image it started");
        let (base, size) = image.info();
        (base as u64, size)
    };
    // Firmware's write-back memory, for an architecture whose boot map types
    // pages by it. Before the exit, while the map can still be asked for; the
    // attributes a descriptor carries do not change across it.
    let write_back = write_back_memory(&system_table);
    let planned = Plan::new(
        gop.as_ref().map(|g| (g.framebuffer, g.framebuffer_size)),
        loader,
        arch::typing(&write_back),
    );
    match &planned {
        Ok(plan) => {
            match plan.scanout() {
                Some((at, len)) => println!(
                    "Scanout: {at:#x}+{len:#x} mapped as the scanout in 2 MiB pages at identity and at \
                     PHYS_OFFSET, in {} page directories",
                    plan.directories().len()
                ),
                None => println!("Scanout: this machine has none"),
            }
            let (at, len) = plan.loader();
            println!("Loader image: {:#x}+{:#x}, mapped at identity as {at:#x}+{len:#x}", loader.0, loader.1);
        }
        Err(why) => println!("Boot map: NO MAP HOLDS THIS MACHINE, {why}"),
    }
    let plan = planned
        .unwrap_or_else(|why| panic!("the boot map cannot hold the scanout and the loader: {why}"));

    // SAFETY: `pt_mem` is the `MAX_PAGES * 4096`-byte, 4096-aligned, zeroed
    // allocation above, and a `Plan` never names more pages than that.
    let pml4_phys = unsafe { build_boot_page_tables(pt_mem, &plan) };
    println!("Boot map: root {pml4_phys:#x}, {BOOT_MAP_BYTES:#x} bytes at identity and at PHYS_OFFSET");

    let kernel_phys = kernel.memory.as_ptr() as u64;
    report_reach("Kernel image", kernel_phys, kernel.memory.len() as u64);

    // An empty cmdline's pointer names nothing and is not reported as reachable.
    match (!cmdline.is_empty()).then_some((cmdline.as_ptr() as u64, cmdline.len() as u64)) {
        None => println!("Parameter buffer: none"),
        Some((at, len)) => report_reach("Parameter buffer", at, len),
    }

    let (gop_framebuffer, gop_framebuffer_size, gop_width, gop_height, gop_stride, gop_pixel_format) =
        match &gop {
            Some(g) => (g.framebuffer, g.framebuffer_size, g.width, g.height, g.stride, g.pixel_format),
            None => (0, 0, 0, 0, 0, 0),
        };

    let (boot_partition_guid, boot_partition_start_lba, boot_partition_blocks, boot_partition_present) =
        match &boot_part {
            Some(p) => (p.guid, p.start_lba, p.blocks, 1),
            None => ([0u8; 16], 0, 0, 0),
        };

    let (root_image_addr, root_image_len, root_partition_guid, root_read_ticks) = root_image.handoff();

    // Built before the exit so the address the kernel is handed is one this
    // loader can still print and refuse on.
    let mut kernel_args = KernelArgs {
        // Filled below: the map is taken after the exit, and sized from a
        // measurement that has to follow the last line printed here.
        memory_map_addr: 0,
        memory_map_size: 0,
        kernel_memory_addr: kernel_phys,
        kernel_memory_size: kernel.memory.len() as u64,
        kernel_stack_addr: kernel.stack_offset as u64,
        kernel_stack_size: kernel.stack_size as u64,
        rsdp_addr,
        kernel_elf_addr: kernel_elf_bytes.as_ptr() as u64,
        kernel_elf_size: kernel_elf_bytes.len() as u64,
        gop_framebuffer,
        gop_framebuffer_size,
        gop_width,
        gop_height,
        gop_stride,
        gop_pixel_format,
        boot_pml4_addr: pml4_phys,
        boot_partition_start_lba,
        boot_partition_blocks,
        boot_partition_guid,
        boot_partition_present,
        log_partition_guid,
        layout: toyos_abi::boot::LAYOUT,
        cmdline_addr: cmdline.as_ptr() as u64,
        cmdline_len: cmdline.len() as u64,
        root_bridge_window_count,
        root_bridge_windows,
        root_image_addr,
        root_image_len,
        root_partition_guid,
        loader_entry_counter: entry_counter,
        loader_handoff_counter: 0,
        root_read_ticks,
        // Read into this struct below, and nowhere else: the kernel zeroes it here.
        loader_seed: [0; toyos_abi::boot::SEED_LEN],
        loader_seed_len: 0,
        wall_clock_secs: 0,
        wall_clock_counter: 0,
        wall_clock_known: 0,
    };
    kernel_args.loader_seed_len = seed::read(&system_table, &mut kernel_args.loader_seed);
    if let Some((civil, counter)) = wall_clock {
        kernel_args.wall_clock_secs = civil.to_unix_secs();
        kernel_args.wall_clock_counter = counter;
        kernel_args.wall_clock_known = 1;
    }
    report_reach(
        "Kernel arguments",
        &kernel_args as *const KernelArgs as u64,
        mem::size_of::<KernelArgs>() as u64,
    );

    kernel_args.loader_handoff_counter = arch::counter();
    println!(
        "Loader counter: {entry_counter} at entry, {} at the handoff",
        kernel_args.loader_handoff_counter,
    );

    // Last, and after every line above: a console write, a FAT write and a
    // handle drop can each add a descriptor, and the margin below is fixed.
    loaderlog::close();
    let mms = system_table.boot_services().memory_map_size();
    let memory_map_entry_count = mms.map_size / mms.entry_size + MAP_MARGIN;
    let mut memory_map = vec::Vec::<MemoryMapEntry>::with_capacity(memory_map_entry_count);

    let (_system_table, uefi_memory_map) = system_table.exit_boot_services(MemoryType::LOADER_DATA);

    // **Nothing below this line may allocate or panic.** Boot services are gone,
    // so the allocator answers null and `println!` dereferences a system table
    // uefi-services has already nulled; either one ends in a panic inside a
    // panic, and a fault with no IDT of our own vectors into firmware's, which
    // dead-loops. The machine then holds the loader's last line on the panel
    // forever and says nothing — which is the failure this loop is written to
    // be incapable of, not merely unlikely to reach.
    uefi_memory_map.entries().for_each(|entry| {
        if memory_map.len() == memory_map.capacity() {
            // A `push` here would grow the vector, and growing it is the death
            // above. What was dropped is not reported: the page that carried
            // that refusal off this boot is gone with the claim, and
            // `issues/the-loaders-truncated-map-refusal-is-executed-by-nothing.md`
            // holds what is owed.
            return;
        }
        memory_map.push(MemoryMapEntry {
            uefi_type: entry.ty.0,
            // Saturating: `overflow-checks` is on in this profile, so a
            // descriptor whose extent does not fit an address would panic here
            // rather than in a caller that could report it.
            start: entry.phys_start,
            end: entry.phys_start.saturating_add(entry.page_count.saturating_mul(PAGE_SIZE as u64)),
        });
    });

    kernel_args.memory_map_addr = memory_map.as_ptr() as u64;
    kernel_args.memory_map_size =
        memory_map.len() as u64 * mem::size_of::<MemoryMapEntry>() as u64;

    #[expect(clippy::disallowed_methods, reason = "`kernel_args.memory_map_addr` hands it to the kernel")]
    mem::forget(memory_map);
    #[expect(clippy::disallowed_methods, reason = "the kernel runs from these pages")]
    mem::forget(kernel.memory);
    #[expect(clippy::disallowed_methods, reason = "`kernel_args.kernel_elf_addr` hands it to the kernel")]
    mem::forget(kernel_elf_bytes);
    #[expect(clippy::disallowed_methods, reason = "`kernel_args.cmdline_addr` hands it to the kernel")]
    mem::forget(cmdline);

    let image = (kernel_phys, kernel_args.kernel_memory_size);
    // SAFETY: `kernel_args.boot_pml4_addr` is the table built above,
    // identity-mapping low memory and this loader's own image (so the code and
    // stack a switch to it runs from stay mapped across it; the stack
    // `kernel_args` lives on was proved inside the low map before the exit) and
    // mapping the same at `PHYS_OFFSET`;
    // the assert before the exit proved the whole kernel image is inside that
    // range, and `image` is that image, relocated, with its entry at
    // `entry_offset`.
    unsafe { arch::enter_kernel(image, kernel.entry_offset as u64, &kernel_args) }
}

/// End a pass that read the black box and boots no kernel, by resetting the
/// machine rather than returning to the boot manager.
///
/// **A UEFI application that returns leaves whatever it registered behind, and
/// the boot manager then unloads its image.** `uefi_services::init` registers a
/// `SIGNAL_EXIT_BOOT_SERVICES` callback that lives here; the next operating
/// system signals that group from inside its own `ExitBootServices`, and
/// firmware calls into memory that is no longer ours.
fn end_this_pass(system_table: &SystemTable<Boot>, exit_event: Option<Event>) -> ! {
    println!("{}", loaderlog::ENDS_AT_CHAIN);
    loaderlog::close_without_a_kernel();
    if let Some(event) = exit_event {
        // After the last line is written: closing it is what stops `println!`
        // being disabled by a callback, not what enables it, but the ordering
        // is the one a reader should not have to check.
        let _ = system_table.boot_services().close_event(event);
    }
    system_table.runtime_services().reset(ResetType::WARM, Status::SUCCESS, None)
}

#[entry]
fn main(handle: Handle, mut system_table: SystemTable<Boot>) -> Status {
    // First, so this reading is firmware's time and none of the loader's.
    let entry_counter = arch::counter();
    let counter_hz = stamp::start();
    let exit_event = uefi_services::init(&mut system_table).unwrap();
    // First, because it covers everything below it: firmware starts a
    // five-minute countdown when it loads an image and resets the machine if
    // the image neither exits boot services nor disables it, and a minute is
    // this project's bound for every watchdog. Reported after the log is open,
    // so the answer is on the stick and not only on the screen.
    let firmware_watchdog =
        system_table.boot_services().set_watchdog_timer(FIRMWARE_WATCHDOG_SECS, WATCHDOG_CODE, None);
    // Before the first line, so the screen holds this loader's lines and none
    // of the firmware's: its logo and its boot manager's text. ClearScreen
    // (UEFI 2.11 §12.4.8) is the console's own clear, which also homes the
    // cursor; said once the log is open.
    let cleared = system_table.stdout().clear();
    // The same sixteen bytes the kernel is handed below, read once, and read
    // before the first line so that no line is only on the screen.
    let log_guid = log_partition_guid(handle, &system_table);
    // Before the log is opened, and before this loader's own allocations can
    // land on the page: whether this pass replaces the last boot's file or
    // appends a report under it is what the page decides, and the boot being
    // reported on has to stay readable.
    let (page, claim_refused) = blackbox::claim(&system_table);
    // The log partition's signature is what a record belongs to: `src/image.rs`
    // mints one per image, so a record another image left in this memory is one
    // this pass clears rather than reports.
    let (finding, stale) = blackbox::harvest(page, log_guid);
    // **The bound on a hang, and it is read before anything is opened**: the
    // count lives on the same volume `loader.log` is about to take exclusively,
    // so this is the one window there is to read and write it.
    let previous = attempt::read(&system_table, &log_guid);
    let retry = match &previous {
        Ok(previous) => attempt::is_the_retry(page.is_some(), finding.is_some(), previous.count),
        Err(_) => false,
    };
    // **How the last boot ended is a fact about the image it ran**: a hang the
    // count caught and every death the page recorded mark that image dead in
    // its slot, and a handover on purpose proves it.
    let ended = match (&finding, retry) {
        (Some(finding), _) => finding.ended,
        (None, true) => Ended::Hung,
        (None, false) => Ended::Unknown,
    };
    // Read here and not where it is used: a hang is a death only for an image
    // above it, and this is the pass that has to know which. A floor refused
    // boots nothing, and is said on the stick before it is said anywhere else.
    let (mut image_floor, floor_notes) = match floor::read(&system_table, &log_guid) {
        Ok(read) => read,
        Err(why) => {
            loaderlog::open(&system_table, &log_guid, false);
            println!("{}", loaderlog::BEGINS_AT);
            println!("{why}");
            panic!("{why}");
        }
    };
    let accounted = record::account(previous.clone().unwrap_or_default(), ended, image_floor.value);
    // Cleared where the last boot is accounted for, and where this pass is
    // about to hand the machine back: both leave the next boot of this image a
    // first attempt, which is what one hand per hang means.
    let next = if finding.is_some() || retry {
        0
    } else {
        attempt::next(previous.as_ref().map_or(0, |previous| previous.count))
    };
    // Names no booted image: the slot this pass boots is written down once it
    // is chosen, and only then.
    let mut record = Record { count: next, ..accounted.record };
    let wrote = attempt::write(&system_table, &log_guid, &record);
    loaderlog::open(&system_table, &log_guid, finding.is_none() && !retry);
    println!("{}", loaderlog::BEGINS_AT);
    match counter_hz {
        Some(hz) => println!("Loader clock: each line opens with the seconds since the counter's zero, at the counter's stated {hz} Hz"),
        None => println!("Loader clock: this CPU states no counter rate, so no line carries the time it was said"),
    }
    if let Err(e) = cleared {
        println!("Screen: firmware's console would not clear ({e}), so its own lines stay above these");
    }
    if let Some(line) = claim_refused {
        println!("{line}");
    }
    if let Some(line) = stale {
        println!("{line}");
    }
    // Said either way, because the bound is only as good as what a reader can
    // see of it: a stick this could not count on is a machine with no bound.
    match (&previous, &wrote) {
        (Err(why), _) | (_, Err(why)) => println!("{ATTEMPTS} {why}, so this boot is not counted and a hang here needs a hand"),
        (Ok(previous), Ok(())) => println!("{ATTEMPTS} this image has had the machine {} time(s) without reporting; now {next}", previous.count),
    }
    if let (Some(died), Some(booted)) = (accounted.died, previous.as_ref().ok().and_then(|p| p.booted)) {
        let mut hex = [0u8; 64];
        println!(
            "Slot {}: its image {} died on its last boot, so no pass boots it again until an update replaces it",
            died.letter(),
            toyos_update::hex(&booted.digest, &mut hex)
        );
    }
    for note in floor_notes {
        println!("{note}");
    }
    // Raised before either end of the chain below: the pass that reads a
    // handover on purpose is the one pass that knows the image proved itself —
    // and raised to the version its slot's signed header carries, verified
    // here, never to the one the record on the disk names.
    if let Some(booted) = accounted.proven {
        match slot::proven(handle, &system_table, &booted) {
            Ok(version) => {
                let to = policy::raised(image_floor.value, version);
                floor::raise(&system_table, &mut image_floor, to);
            }
            Err(why) => println!("Anti-rollback floor: not raised, because the proven image is not verified: {why}"),
        }
    }
    if retry {
        // **The hang, and the only bound there is on one.** The last boot of
        // this image was handed the machine and never reported — no panic, no
        // fault, no deliberate handover — and the black box is empty, which is
        // what a power cut leaves. Booting the same kernel again is the loop the
        // owner is already in, so this pass boots none: `BootNext` is left alone
        // and the firmware's own boot order takes the machine.
        println!("{HUNG_WITHOUT_A_RECORD}");
        end_this_pass(&system_table, exit_event);
    }
    if let Some(finding) = finding {
        for line in &finding.lines {
            println!("{line}");
        }
        // The file and not the console: `blackbox::tail` says what the
        // firmware's own scroll costs this machine, and every reader of these
        // lines reads them off the stick.
        for line in &finding.filed {
            loaderlog::line(format_args!("{}{line}", stamp::now()));
        }
        if finding.ends_the_chain {
            // The last boot is accounted for, so this pass boots no kernel.
            end_this_pass(&system_table, exit_event);
        }
    }
    match firmware_watchdog {
        Ok(()) => println!(
            "Firmware watchdog: {FIRMWARE_WATCHDOG_SECS} s, until ExitBootServices disables it"
        ),
        Err(e) => println!(
            "Firmware watchdog: firmware refused {FIRMWARE_WATCHDOG_SECS} s ({e}), so a hang in \
             this loader needs a hand on the button"
        ),
    }

    // Find ACPI 2.0 RSDP from UEFI configuration table
    let rsdp_addr = system_table
        .config_table()
        .iter()
        .find(|entry| entry.guid == ACPI2_GUID)
        .map(|entry| entry.address as u64)
        .expect("ACPI 2.0 RSDP not found in UEFI config table");
    println!("RSDP address: {:#x}", rsdp_addr);

    let boot_part = boot_partition(handle, &system_table);
    match &boot_part {
        Some(p) => println!(
            "Boot partition: LBA {}+{} signature {:02x?}",
            p.start_lba, p.blocks, p.guid
        ),
        None => println!("Boot partition: this machine has none"),
    }

    println!("Log partition: signature {:02x?}", log_guid);

    // Every byte the kernel is handed below — its ELF, its parameter and ROOT —
    // is one the chosen slot's signed header names. ROOT is read before the
    // kernel is loaded and not after: it is the allocation the image pages come
    // from, and a slot whose ROOT is refused has no use for the kernel's.
    let chosen = slot::choose(handle, &system_table, image_floor.value, &record)
        .unwrap_or_else(|why| panic!("Slots: {why}"));
    record.booted = Some(Booted { slot: chosen.which, version: chosen.version, digest: chosen.digest });
    match attempt::write_chosen(&log_guid, &record) {
        Ok(()) => println!("{ATTEMPTS} slot {}'s image is written down as the one this pass boots", chosen.which.letter()),
        Err(why) => println!(
            "{ATTEMPTS} {why}, so a death of slot {}'s image is not seen by the next pass",
            chosen.which.letter()
        ),
    }
    let kernel_bytes = chosen.kernel;
    println!("Kernel: {} bytes", kernel_bytes.len());

    // The words naming the slot and the page are appended to what the slot
    // carried, because each is a fact only this loader has: the kernel is
    // handed one line and reads its own parameters out of it.
    let mut cmdline = chosen.cmdline;
    let mut append = |word: &str| {
        if !cmdline.is_empty() {
            cmdline.push(b',');
        }
        cmdline.extend_from_slice(word.as_bytes());
    };
    append(&alloc::format!("{}{}", toyos_abi::boot::SLOT_PARAM, chosen.which.letter()));
    if let Some((refused, why)) = chosen.refused {
        append(&alloc::format!("{}{}:{}", toyos_abi::boot::SLOT_REFUSED_PARAM, refused.letter(), why.word()));
    }
    if let Some(word) = blackbox::param(page) {
        append(&word);
    }
    let params = core::str::from_utf8(&cmdline)
        .unwrap_or_else(|e| panic!("slot {}'s cmdline is not UTF-8: {e}", chosen.which.letter()));
    println!("Boot parameter: {params:?}");

    println!("Loading kernel elf...");
    let loaded_kernel = load_kernel_elf(&kernel_bytes);

    // Query UEFI GOP before exiting boot services
    let gop = query_gop(&system_table);

    // Read once, outside the watchdog's bound: the page is stamped with it, and
    // the kernel carries it forward to its boot on the counter read beside it.
    let wall_clock = match wallclock::now(&system_table) {
        Ok((civil, counter)) => {
            println!("Wall clock: firmware's GetTime reads {civil} UTC at counter {counter}");
            Some((civil, counter))
        }
        Err(why) => {
            println!("Wall clock: {why}, so the kernel is handed none");
            None
        }
    };

    // The page says a kernel is running, and `BootNext` says this loader gets the
    // machine again however that kernel ends.
    blackbox::arm(page, wall_clock.map_or(0, |(civil, _)| civil.to_unix_secs()), log_guid);
    bootnext::point_at_us(handle, &system_table);

    // The last act before the jump, so the smallest possible span of this loader
    // is inside the bound: everything above it can still be reported, and a hang
    // between here and the kernel's own arm is what the bound is for.
    watchdog::arm(&system_table, rsdp_addr, params);

    println!("Starting kernel...");
    start_kernel(loaded_kernel, kernel_bytes, cmdline, rsdp_addr, gop, boot_part, log_guid, chosen.root, entry_counter, wall_clock, system_table);
}
