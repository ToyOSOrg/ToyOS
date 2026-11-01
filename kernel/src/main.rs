#![no_std]
#![no_main]
// Every unsafe block here carries a SAFETY: comment unless a `mod` line
// below is exempted.
#![warn(clippy::undocumented_unsafe_blocks)]
extern crate alloc;

/// Debugger spin gate: LLDB releases it via `expr -- *(bool*)&DEBUG_WAIT = false`.
#[no_mangle]
#[cfg(feature = "debug-wait")]
static DEBUG_WAIT: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(true);

pub use mm::{UserAddr, DirectMap, PHYS_OFFSET};

mod invalidation;
mod seqlock;
mod shootdown;
mod tlb_tags;
mod cpu_local;
mod cpu_slice;
mod sleeplock;
mod smp;
mod smp_roster;
mod sync;
mod hasher;
mod id_map;

// No `mod` line below carries an `#[allow(clippy::undocumented_unsafe_blocks)]`.
mod arch;
mod drivers;

#[macro_use]
mod log;
mod actuator;
mod params;
mod blackbox;
mod deadline;
mod quiesce;
mod random;
mod hardlockup;
mod mm;
mod panic;
mod panic_reboot;
mod power;

mod inventory;
mod loaded;
mod rollback;
mod bootimage;
mod file_cache;
#[cfg(feature = "boot-actuators")]
#[cfg(feature = "boot-actuators")]
mod revoke_selftest;
mod tmpfs;
mod file_backing;
mod fs_rename;
mod vfs;
mod elf;
mod symbols;
mod process;
mod loader;
mod scheduler;
mod sched;
mod hw;
mod idle;
mod iommu;
mod preempt;
mod counters;
mod census;
mod irq_census;
#[cfg(feature = "mask-windows")]
mod windows;
mod trace;
mod time;
mod clock;

mod watch;
mod object;
mod inbox;
mod pipe;
mod frame_bytes;
mod message;

mod device;
mod pcidev;
mod isa;
mod user_ptr;
mod syscall;

/// Nested generic forces a demangled symbol wider than the console grid.
#[cfg(feature = "boot-actuators")]
mod late_panic {
    pub struct Nest<T>(core::marker::PhantomData<T>);

    impl<T> Nest<T> {
        #[inline(never)]
        pub fn on_screen_console_check() -> ! {
            panic!("test-late-panic: on-screen console check");
        }
    }
}

use alloc::boxed::Box;
use arch::{cpu, percpu};
use drivers::{acpi, gop, pci, serial};
use toyos_abi::boot::{KernelArgs, MemoryMapEntry};
use toyos_rootimage::handoff::{held, Descriptor};

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    cpu::disable_interrupts();
    // Before the first record of the report: no holder carries it off the UART.
    log::console::on_the_uart_for_good();
    // A handler's own panic reports through locks like any other.
    #[cfg(feature = "mask-windows")]
    windows::stand_down();

    // Must run first: captures state for a possible second panic, declining if this CPU is already inside one.
    panic::record_panic(info);

    // Reentry guard, checked before any fallible access — else a panic here recurses until the stack meets the heap.
    let depth = panic::depth_slot();
    if depth.fetch_add(1, core::sync::atomic::Ordering::SeqCst) > 0 {
        // UART only: percpu and the log path are what just panicked.
        panic::last_words("PANIC REENTRY: CPU halted", None, info, false);
        // Off the record like the report above it: the log path is what just panicked here.
        let bound = panic_reboot::arm(false);
        // No capture(): the outer panic's snapshot is the one worth showing.
        // render() is safe by construction here: a fault inside the renderer itself would find the fatal panel already claimed, and return without touching a pixel.
        if drivers::panic_console::render() {
            drivers::panic_console::hold_the_panel(bound);
        }
        cpu::halt();
    }

    // Early boot: percpu not ready (single CPU at this point), and neither is the calibrated clock — the bound comes off CPUID there.
    if !log::PERCPU_READY.load(core::sync::atomic::Ordering::Relaxed) {
        alert!("EARLY PANIC: {}", info);
        // Before the capture, so the arm line is the panel's last one.
        let bound = panic_reboot::arm(true);
        // Halts directly instead of via halt_all_cpus: the exception table is not loaded yet, so a renderer fault would find firmware's.
        drivers::panic_console::capture();
        // SAFETY: no other writer can be mid-transmission — IF is clear here and every other CPU is about to halt.
        unsafe { drivers::serial::panic_flush(); }
        // Flush before render: the serial report survives even if render then faults.
        if drivers::panic_console::render() {
            drivers::panic_console::hold_the_panel(bound);
        }
        cpu::halt();
    }

    let prev = percpu::swap_fault_state(percpu::CpuFaultState::Panic);
    if prev != percpu::CpuFaultState::Normal {
        // Escalate: reentry depth is zero here, so this landed on a fatal exception or page fault no handler was inside.
        panic::last_words("DOUBLE PANIC", Some(prev), info, true);
        panic::halt_all_cpus();
    }

    arch::trap::report_panic(info, cpu::frame_pointer());

    drivers::panic_console::capture();
    // SAFETY: IF is clear on this CPU and every other one halts before anything else can write the port.
    unsafe { drivers::serial::panic_flush(); }

    panic::halt_all_cpus();
}

/// The EDID block the loader handed over and its length, which the loader
/// writes as a whole block or nothing; the bytes are not read here.
fn gop_edid(args: &KernelArgs) -> ([u8; toyos_abi::boot::EDID_BLOCK], u32) {
    match args.gop_edid_len {
        0 => ([0; toyos_abi::boot::EDID_BLOCK], 0),
        len if len == toyos_abi::boot::EDID_BLOCK as u64 => (args.gop_edid, len as u32),
        other => panic!("the loader wrote {other} as gop_edid_len, which is 0 or {}", toyos_abi::boot::EDID_BLOCK),
    }
}

/// The boot from power-on, off the loader's [`cpu::counter`] readings and
/// `complete`'s, at the clock's rate. The first span is firmware's time since
/// the counter started.
fn report_power_on(args: &KernelArgs, complete: u64) {
    arch::boot::report_counter_origin();
    let (entry, handoff) = (args.loader_entry_counter, args.loader_handoff_counter);
    if handoff < entry || complete < handoff {
        log!(
            "boot: the counter went backwards: {entry} at the loader's entry, {handoff} at its handoff, \
             {complete} at Boot: complete"
        );
        return;
    }
    let ms = |ticks: u64| clock::nanos_of_ticks(ticks) / 1_000_000;
    log!(
        "boot: power-on to loader {} ms, loader {} ms (ROOT read {} ms), kernel to Boot: complete {} ms",
        ms(entry),
        ms(handoff - entry),
        ms(args.root_read_ticks),
        ms(complete - handoff),
    );
}

/// Says where this boot's log can be read, on the last surface still showing it once userland owns the screen.
fn report_log_destination() {
    // Kernel-side because panic_console owns the panel; `/log` is its file server's, which says whether it mounted.
    // ASCII only: the panel's font renders anything outside 0x20..=0x7E as a dot.
    match drivers::serial::has_console() {
        true => log!("log: this boot is on the console, and on /log once its file server mounts it"),
        false => log!("log: no serial console - this boot is on the screen, and on /log once its file server mounts it"),
    }
}

/// The architecture's entry calls this once, on the kernel's own stack, with
/// the loader's arguments.
/// # Safety
/// `loader_args` is the loader's live [`KernelArgs`], which nothing else names, and nothing has run before this.
pub(crate) unsafe extern "C" fn kernel_main(loader_args: &mut KernelArgs) -> ! {
    // Before the first record, which is stamped at the rate it states.
    clock::state();
    // Copied onto the kernel stack: the original lives on the UEFI stack, unreachable once mm::init drops the identity map.
    let mut kernel_args = *loader_args;

    let entry_count = kernel_args.memory_map_size as usize / core::mem::size_of::<MemoryMapEntry>();
    let maps = core::slice::from_raw_parts(
        DirectMap::from_phys(kernel_args.memory_map_addr).as_ptr::<MemoryMapEntry>(),
        entry_count,
    );

    arch::boot::before_panel();

    // Before serial::init: the screen may be the only surviving channel if serial::init itself faults.
    drivers::panic_console::arm(&kernel_args, maps);
    // Before the first field a layout change can move; `rsdp_addr` sits below
    // the word, so the refusal reaches the UART.
    if kernel_args.layout != toyos_abi::boot::LAYOUT {
        serial::init(kernel_args.rsdp_addr);
        panic!(
            "boot: the loader wrote KernelArgs layout {:#x} and this kernel reads layout {:#x}",
            kernel_args.layout,
            toyos_abi::boot::LAYOUT
        );
    }
    // Beside it, and out of the raw buffer: a panic between here and
    // `params::init` — inside `serial::init`, or on the parameter line's own
    // UTF-8 check — is one the page has to carry past the reset, and neither
    // the console nor that line has been decided yet.
    blackbox::arm(if kernel_args.cmdline_len == 0 {
        &[]
    } else {
        core::slice::from_raw_parts(
            DirectMap::from_phys(kernel_args.cmdline_addr).as_ptr::<u8>(),
            kernel_args.cmdline_len as usize,
        )
    });

    serial::init(kernel_args.rsdp_addr);

    // After both channels exist, before the first actuator site.
    // cmdline_len==0 is checked first: an empty bootloader Vec has no backing allocation to point at.
    let cmdline = if kernel_args.cmdline_len == 0 {
        ""
    } else {
        core::str::from_utf8(core::slice::from_raw_parts(
            DirectMap::from_phys(kernel_args.cmdline_addr).as_ptr::<u8>(),
            kernel_args.cmdline_len as usize,
        ))
        .expect("the boot parameter is not UTF-8")
    };
    // Both readings of it here: the parameter is in no reserved region, so
    // `mm::init` may hand that memory out and neither may hold a borrow.
    params::init(cmdline);
    deadline::claim(cmdline);
    actuator::init(cmdline);
    let (boot_images, [supervisor_image, fileserver_image, root_image]) = bootimage::init(&kernel_args, maps);

    arch::boot::after_console(&kernel_args, maps);

    // percpu, the allocator and our own paging aren't up yet, so a fault here only reaches the early-panic branch.
    if actuator::test_early_panic() {
        panic!("test-early-panic: on-screen console check");
    }

    #[cfg(feature = "debug-wait")]
    {
        log!("debug: waiting for debugger — set DEBUG_WAIT=false to continue");
        while DEBUG_WAIT.load(core::sync::atomic::Ordering::Relaxed) {
            core::hint::spin_loop();
        }
    }

    // Split into six records: KernelArgs' derived Debug is the one message that exceeds the log's per-record bound.
    log!(
        "boot: memory map {:#x}+{:#x}, kernel {:#x}+{:#x}, stack image+{:#x}+{:#x}",
        kernel_args.memory_map_addr, kernel_args.memory_map_size,
        kernel_args.kernel_memory_addr, kernel_args.kernel_memory_size,
        kernel_args.kernel_stack_addr, kernel_args.kernel_stack_size
    );
    log!(
        "boot: kernel elf {:#x}+{:#x}, rsdp {:#x}, boot pml4 {:#x}",
        kernel_args.kernel_elf_addr, kernel_args.kernel_elf_size,
        kernel_args.rsdp_addr, kernel_args.boot_pml4_addr
    );
    log!(
        "boot: gop {:#x}+{:#x} {}x{} stride {} format {}",
        kernel_args.gop_framebuffer, kernel_args.gop_framebuffer_size,
        kernel_args.gop_width, kernel_args.gop_height,
        kernel_args.gop_stride, kernel_args.gop_pixel_format
    );
    log!(
        "boot: boot partition present={} lba {} +{} blocks guid {:02x?}",
        kernel_args.boot_partition_present, kernel_args.boot_partition_start_lba,
        kernel_args.boot_partition_blocks, kernel_args.boot_partition_guid
    );
    log!("boot: log partition guid {:02x?}", kernel_args.log_partition_guid);
    log!(
        "boot: cmdline {:#x}+{}",
        kernel_args.cmdline_addr, kernel_args.cmdline_len
    );
    // Before `mm::init`, which may hand the parameter's memory out. This record
    // is how a slot that died or was refused reaches the next boot's `/log`.
    match params::slot(cmdline) {
        (Some(slot), None) => log!("{} {slot}, the one the slot table marks", params::SLOT_RECORD),
        (Some(slot), Some(refused)) => match refused.split_once(':') {
            Some((marked, why)) => log!("{} {slot}, because the marked slot {marked} was refused: {why}", params::SLOT_RECORD),
            None => log!("{} {slot}, because the marked slot was refused: {refused}", params::SLOT_RECORD),
        },
        (None, _) => log!("{} none: the loader named no slot", params::SLOT_RECORD),
    }

    let kernel_elf = core::slice::from_raw_parts(
        DirectMap::from_phys(kernel_args.kernel_elf_addr).as_ptr::<u8>(),
        kernel_args.kernel_elf_size as usize,
    );
    // After the boot's own lines, so its refusal reaches a channel that says
    // what machine this is, and while the loader's arguments are still mapped.
    random::key(loader_args, &mut kernel_args);
    let kernel_args = &kernel_args;

    // `kernel_stack_addr` is an offset into the image, so the image's region is what keeps the stack.
    assert!(
        kernel_args.kernel_stack_addr.checked_add(kernel_args.kernel_stack_size)
            .is_some_and(|end| end <= kernel_args.kernel_memory_size),
        "boot: the loader put the stack at image+{:#x}+{:#x}, past the {:#x}-byte image",
        kernel_args.kernel_stack_addr, kernel_args.kernel_stack_size, kernel_args.kernel_memory_size
    );
    let loader = [
        mm::Region { start: kernel_args.kernel_memory_addr, end: kernel_args.kernel_memory_addr + kernel_args.kernel_memory_size },
        mm::Region { start: kernel_args.kernel_elf_addr, end: kernel_args.kernel_elf_addr + kernel_args.kernel_elf_size },
        // The loader's black-box page, which is ordinary `LoaderData` and so
        // memory the allocator would otherwise hand out. Empty on a boot whose
        // parameter line names none.
        blackbox::reserved_region(),
        // The boot images (`bootimage`), `LoaderData` like the black box's page.
        supervisor_image,
        fileserver_image,
        root_image,
        // Firmware's memory map as the loader copied it, which `mm` keeps
        // (`mm::firmware_map`): `LoaderData` too.
        mm::Region { start: kernel_args.memory_map_addr, end: kernel_args.memory_map_addr + kernel_args.memory_map_size },
        // The microcode patch every AP loads after `mm::init`, `LoaderData`
        // like the others. Empty where the loader handed none.
        mm::Region { start: kernel_args.microcode_addr, end: kernel_args.microcode_addr + kernel_args.microcode_len },
    ];
    // A region the loader did not allocate withholds memory nothing uses, so one the firmware map does not hold as `LoaderData` is refused.
    // Block 1: the ELF region (`kernel_elf_addr`+`kernel_elf_size`) is not page-aligned.
    for region in loader.iter().filter(|r| r.start < r.end) {
        assert!(
            held(
                maps.iter().map(|e| Descriptor { ty: e.uefi_type, start: e.start, end: e.end }),
                toyos_bootmap::EFI_LOADER_DATA,
                region.start,
                region.end - region.start,
                1,
            )
            .is_some(),
            "boot: reserving {:#x}..{:#x}, which no LoaderData descriptor in the firmware map holds",
            region.start, region.end
        );
    }
    // The architecture's own page is not a loader allocation, so it is named
    // here rather than folded into `loader` above. Destructuring `loader` by
    // name, rather than indexing it, means a region added to `loader` fails
    // to compile here instead of compiling and being silently dropped from
    // what `mm::init` withholds.
    let [image, elf, black_box, supervisor, fileserver, root, map, microcode] = loader;
    let reserved = [image, elf, black_box, supervisor, fileserver, root, map, microcode, arch::boot::reserved()];
    // A region another one shares is two allocations the loader made of one
    // byte, and whichever writes it second corrupts the first.
    let extents = reserved.map(|r| r.start..r.end);
    if let Some((a, b)) = toyos_rootimage::handoff::overlapping(&extents) {
        panic!(
            "boot: the reserved regions {:#x}..{:#x} and {:#x}..{:#x} share memory",
            reserved[a].start, reserved[a].end, reserved[b].start, reserved[b].end
        );
    }
    let layout = toyos_elf::Layout::parse(kernel_elf, arch::ELF_MACHINE)
        .unwrap_or_else(|e| panic!("boot: the loader's kernel.elf: {e}"));
    let image = mm::Image::new(&layout, image);

    // Before the first hash container, `mm::init`'s address space.
    hasher::seed();

    mm::init(maps, &reserved, &image);
    drivers::panic_console::remap();

    // Before the first table is decoded for its contents: what a machine owner
    // reads off a refusal below is which tables the firmware published at all.
    acpi::inventory(kernel_args.rsdp_addr);

    // Before this CPU's own per-CPU block: everything sized per CPU is sized
    // here, once, while it runs alone.
    smp::size(acpi::madt_processors(kernel_args.rsdp_addr));
    let platform = arch::boot::interrupts(kernel_args.rsdp_addr);
    counters::bring_up();
    symbols::set_kernel_base(kernel_args.kernel_memory_addr);
    symbols::load_kernel(kernel_elf, mm::PHYS_OFFSET + kernel_args.kernel_memory_addr);

    arch::boot::clock(kernel_args);
    trace::enable();
    arch::boot::timer();
    // After both halves of what it needs: a counter period to convert its bound
    // with, and a timer whose every tick polls it.
    deadline::start();

    boot_phase!("CPU ready", 0);

    let t_periph = clock::nanos_since_boot();

    let ecam_windows = acpi::ecam_windows(kernel_args.rsdp_addr);
    let pci_devices = pci::enumerate(&ecam_windows);
    // Before any driver `init`: this sizes every BAR on the machine, and the
    // spec's probe takes memory decode off the function it is sizing for the
    // length of it. Nothing has bound yet, so nothing is mid-transfer.
    pcidev::publish(&pci_devices, toyos_acpi::SEGMENT_GROUP, kernel_args.root_bridge_windows());
    #[cfg(feature = "boot-actuators")]
    if actuator::pci_cap_selftest() {
        drivers::pci::cap_selftest();
    }
    // After ACPI is readable and PCI is enumerable, before any driver `init`: each enumerated device needs a context entry before it can DMA.
    // Refuses nothing — a machine with no usable IOMMU boots exactly as one without it.
    iommu::init(
        kernel_args.rsdp_addr,
        &pci_devices,
        pcidev::declared().as_ref(),
        (kernel_args.kernel_memory_addr, kernel_args.kernel_memory_addr + kernel_args.kernel_memory_size),
    );
    // Before storage and everything under it: what it covers is the rest of this
    // boot, and a wedge down there is the reason to have one.
    arch::watchdog::init(&pci_devices);
    file_cache::init();
    loaded::init(kernel_args);

    boot_phase!("peripherals ready", t_periph);

    let t_subsys = clock::nanos_since_boot();

    arch::boot::start_other_cpus(&platform, kernel_args);
    vfs::init();
    process::init();
    scheduler::init();

    vfs::lock().mount(&["tmp"], Box::new(crate::tmpfs::TmpFs::new()));

    boot_phase!("subsystems ready", t_subsys);

    // Off the loader's images, so nothing from here to the supervisor's
    // spawn asks a disk for anything: every storage driver comes up after it.
    // The supervisor reads /system/etc/system.manifest itself; the boot config never names the program it starts.
    let pid = process::spawn_supervisor(bootimage::objects(boot_images));
    log!("spawned the supervisor pid={pid}");

    #[cfg(feature = "boot-actuators")]
    if actuator::revoked_backing_selftest() {
        revoke_selftest::run();
    }

    let t_devices = clock::nanos_since_boot();

    // First in the device phase, after storage: its lines are the diagnostic
    // boot's answer for a dead keyboard, and a panel shows the log's tail.
    arch::boot::platform_devices(&platform, kernel_args.rsdp_addr);

    #[cfg(feature = "boot-actuators")]
    arch::boot::interrupt_selftests();

    if kernel_args.gop_framebuffer != 0 {
        log!("GPU: using UEFI GOP");
        match gop::init(
            kernel_args.gop_framebuffer,
            kernel_args.gop_framebuffer_size,
            kernel_args.gop_width,
            kernel_args.gop_height,
            kernel_args.gop_stride,
            kernel_args.gop_pixel_format,
            gop_edid(kernel_args),
        ) {
            Some(screen) => device::set_framebuffer_info(screen),
            None => log!("GPU: none this boot, running headless"),
        }
    } else {
        log!("GPU: the firmware handed over no framebuffer");
    }

    boot_phase!("devices ready", t_devices);

    // After every kernel driver took its functions from firmware, before
    // userland runs and asks for a claim.
    iommu::drivers_settled();

    // Once the boot's own mappings are made: every later kernel leaf is
    // written by the same constructors.
    #[cfg(feature = "boot-actuators")]
    if actuator::kernel_wx_leaf() {
        let frame = mm::pmm::alloc_page().expect("kernel-wx-leaf: no frame to stage on");
        let at = mm::paging::stage_writable_executable(frame.direct_map().phys());
        log!("kernel-wx-leaf: the leaf at {at:#x} is writable and executable");
        #[expect(clippy::disallowed_methods, reason = "the audit below ends this boot on the leaf staged over it")]
        core::mem::forget(frame);
    }
    mm::audit_wx(&image);

    report_log_destination();
    let complete = cpu::counter();
    boot_phase!("complete", 0);
    report_power_on(kernel_args, complete);

    #[cfg(feature = "boot-actuators")]
    if actuator::panel_painter_stalls() {
        drivers::panic_console::stall::inside_the_latch();
    }

    #[cfg(feature = "boot-actuators")]
    if actuator::test_late_panic() {
        late_panic::Nest::<late_panic::Nest<late_panic::Nest<late_panic::Nest<
            late_panic::Nest<late_panic::Nest<late_panic::Nest<late_panic::Nest<
            late_panic::Nest<late_panic::Nest<()>>>>>>>>>>::on_screen_console_check();
    }

    // Last thing before enter_idle_loop: nothing can run before it, and a klogd spawned earlier would idle through phases 5-7 with no drainer.
    log::console::start();

    smp::set_ready();

    // After the release, because a shootdown waits on CPUs that are not
    // answering until it; before the idle loop, because nothing else may be
    // running while the distribution is measured.
    #[cfg(feature = "boot-actuators")]
    if actuator::tlb_shootdown_bench() {
        arch::tlb::bench();
    }

    crate::scheduler::enter_idle_loop();
}

