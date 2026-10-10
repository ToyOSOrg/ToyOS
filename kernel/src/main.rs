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

mod keyboard;
mod mouse;
#[cfg(feature = "boot-actuators")]
mod input_merge_test;
#[cfg(feature = "boot-actuators")]
mod usb_gate;
#[cfg(feature = "boot-actuators")]
mod sched_gate;
mod block;
mod gpt;
mod inventory;
mod rollback;
mod rootfs;
mod file_cache;
#[cfg(feature = "boot-actuators")]
mod leak_selftest;
#[cfg(feature = "boot-actuators")]
mod revoke_selftest;
mod tmpfs;
mod file_backing;
mod bcachefs_adapter;
mod fs_rename;
mod vfs;
mod elf;
mod symbols;
mod process;
mod loader;
mod scheduler;
mod sched;
mod hw;
mod iommu;
mod preempt;
mod counters;
mod census;
mod irq_census;
#[cfg(feature = "mask-windows")]
mod windows;
mod irq_ring;
mod trace;
mod time;
mod clock;

mod watch;
mod object;
mod inbox;
mod pipe;

mod device;
mod pcidev;
mod isa;
mod gpu;
mod user_ptr;
mod vma;
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
use drivers::{acpi, gop, pci, serial, virtio_console, virtio_gpu, virtio_sound, xhci};
use toyos_abi::boot::{KernelArgs, MemoryMapEntry};
use toyos_rootimage::handoff::{held, Descriptor};

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    cpu::disable_interrupts();
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

fn register_gpu(driver: Box<dyn gpu::Gpu>, info: gpu::GpuInfo) {
    gpu::register(driver, info);
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
    // Kernel-side because panic_console owns the panel; logkeeper reports which file it opened separately.
    // Whether the partition is on a disk this kernel reads, not whether its file server mounted it:
    // that server says so itself, and this kernel mounts nothing but ROOT.
    let console = drivers::serial::has_console();
    let has_log = match gpt::log_place() {
        gpt::LogPlace::Driven => true,
        gpt::LogPlace::Unnamed | gpt::LogPlace::Absent => false,
        gpt::LogPlace::Undriven => {
            log!("log: /log is on a disk this kernel does not drive; its file server says whether it mounted");
            return;
        }
    };
    // ASCII only: the panel's font renders anything outside 0x20..=0x7E as a dot.
    match (console, has_log) {
        (true, true) => log!("log: this boot is on the console and on /log"),
        (false, true) => log!("log: no serial console - this boot is on /log and on the screen"),
        // alert! reddens the panel's Level for exactly the two states that leave no account of this boot anywhere.
        (true, false) => {
            alert!("log: no /log - this boot is on the console only, and nothing outlives the power")
        }
        (false, false) => {
            alert!("log: no serial console and no /log - this boot is on this screen and nowhere else")
        }
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

    blackbox::step("kernel_main: armed; serial::init");
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
    let root_image = rootfs::init(cmdline, &kernel_args, maps);

    blackbox::step("kernel_main: after_console");
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
    // ROOT's name hashes every file it carries, `/system/etc/os-release` among
    // them: the build, named before anything is mounted.
    match toyos_abi::boot::root_uuid(cmdline).map(bcachefs::FsUuid::parse) {
        Some(Some(root)) => log!("boot: root={root}"),
        Some(None) => log!("boot: root= names no filesystem this kernel can parse"),
        None => log!("boot: the cmdline carries no root="),
    }
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
    blackbox::step("kernel_main: random::key");
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
        // ROOT's image, `LoaderData` like the black box's page. Empty on a
        // boot the loader handed none.
        root_image,
        // Firmware's memory map as the loader copied it, which `mm` keeps
        // (`mm::firmware_map`): `LoaderData` too.
        mm::Region { start: kernel_args.memory_map_addr, end: kernel_args.memory_map_addr + kernel_args.memory_map_size },
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
    let [image, elf, black_box, root, map] = loader;
    let reserved = [image, elf, black_box, root, map, arch::boot::reserved()];

    // Before the first hash container, `mm::init`'s address space.
    hasher::seed();

    blackbox::step("kernel_main: mm::init");
    mm::init(maps, &reserved);
    blackbox::step("kernel_main: panic_console::remap");
    drivers::panic_console::remap();
    blackbox::step("kernel_main: acpi::inventory");

    // Before the first table is decoded for its contents: what a machine owner
    // reads off a refusal below is which tables the firmware published at all.
    acpi::inventory(kernel_args.rsdp_addr);

    let platform = arch::boot::interrupts(kernel_args.rsdp_addr);
    blackbox::step("kernel_main: counters::bring_up");
    counters::bring_up();
    blackbox::step("kernel_main: symbols");
    symbols::set_kernel_base(kernel_args.kernel_memory_addr);
    if !kernel_elf.is_empty() {
        symbols::load_kernel(kernel_elf, mm::PHYS_OFFSET + kernel_args.kernel_memory_addr);
    }

    blackbox::step("kernel_main: clock");
    arch::boot::clock(kernel_args);
    trace::enable();
    blackbox::step("kernel_main: timer");
    arch::boot::timer();
    blackbox::step("kernel_main: deadline::start");
    // After both halves of what it needs: a TSC period to convert its bound
    // with, and a timer whose every tick polls it.
    deadline::start();

    boot_phase!("CPU ready", 0);

    let t_periph = clock::nanos_since_boot();

    let ecam_windows = acpi::ecam_windows(kernel_args.rsdp_addr);
    blackbox::step("kernel_main: pci::enumerate");
    let pci_devices = pci::enumerate(&ecam_windows);
    blackbox::step("kernel_main: pcidev::publish");
    // Every window is on one segment group; a machine with no window has no
    // function to name one for.
    let pci_segment = ecam_windows.first().map_or(0, |window| window.segment());
    // Before any driver `init`: this sizes every BAR on the machine, and the
    // spec's probe takes memory decode off the function it is sizing for the
    // length of it. Nothing has bound yet, so nothing is mid-transfer.
    pcidev::publish(&pci_devices, pci_segment, maps, kernel_args.root_bridge_windows());
    #[cfg(feature = "boot-actuators")]
    if actuator::pci_cap_selftest() {
        drivers::virtio::cap_selftest();
    }
    // After ACPI is readable and PCI is enumerable, before any driver `init`: each enumerated device needs a context entry before it can DMA.
    // Refuses nothing — a machine with no usable IOMMU boots exactly as one without it.
    blackbox::step("kernel_main: iommu::init");
    iommu::init(kernel_args.rsdp_addr, &pci_devices, kernel_args.root_bridge_windows());
    blackbox::step("kernel_main: watchdog, file_cache, gpt");
    // Before storage and everything under it: what it covers is the rest of this
    // boot, and a wedge down there is the reason to have one.
    arch::watchdog::init(&pci_devices);
    file_cache::init();
    gpt::init(kernel_args);

    boot_phase!("peripherals ready", t_periph);

    let t_subsys = clock::nanos_since_boot();

    blackbox::step("kernel_main: start_other_cpus");
    arch::boot::start_other_cpus(&platform, kernel_args);
    blackbox::step("kernel_main: other cpus started");
    vfs::init();
    process::init();
    scheduler::init();
    // Task-less half of the operation-nesting gate: this boot phase has no current task, so it establishes into the per-CPU slot.
    #[cfg(feature = "boot-actuators")]
    if actuator::sched_operation_nesting() {
        sched_gate::run("boot");
    }
    pipe::init();

    // ROOT is the loader's image in memory, so nothing from here to the supervisor's
    // spawn asks a disk for anything: every storage driver comes up after it.
    use vfs::UserAccess;
    let root_fs = rootfs::mount();
    vfs::lock().mount(
        &["system"],
        Box::new(bcachefs_adapter::ReadOnlyBcacheFsAdapter::new(root_fs)),
        UserAccess::KernelOnly,
    );
    vfs::lock().mount(&["tmp"], Box::new(crate::tmpfs::TmpFs::new()), UserAccess::ReadWrite);

    boot_phase!("subsystems ready", t_subsys);

    // The supervisor reads /system/etc/system.manifest itself; the boot config never names the program it starts.
    let pid = process::spawn_supervisor();
    log!("spawned {} pid={pid}", process::SUPERVISOR_PATH);

    // The proof the boot up to here needed no disk: ROOT and the supervisor's image both
    // came out of memory.
    log!("{} {}", rootfs::SUPERVISOR_WITHOUT_A_DISK, block::census::commands_issued());

    // After the supervisor's spawn and before it runs: nothing runs a task until
    // `smp::set_ready` below. This kernel mounts no disk: `/apps`, `/config`,
    // `/home`, `/state`, `/log` and `/boot` are file servers' (`/system/bin/fileserver`),
    // and an NVMe controller is `/system/bin/diskserver`'s. The USB disks are still
    // this kernel's, served to their file servers as partition claims, until
    // usbd drives the controller.
    let t_storage = clock::nanos_since_boot();

    xhci::init(&pci_devices);
    // After xhci::init: a USB-booted disk doesn't exist until the controller binds it.
    gpt::probe_usb_disks();
    rootfs::hold_source();

    #[cfg(feature = "boot-actuators")]
    if actuator::leak_rollback_selftest() {
        leak_selftest::run();
    }
    #[cfg(feature = "boot-actuators")]
    if actuator::revoked_backing_selftest() {
        revoke_selftest::run();
    }

    boot_phase!("storage ready", t_storage);

    let t_devices = clock::nanos_since_boot();

    // First in the device phase, after storage: its lines are the diagnostic
    // boot's answer for a dead keyboard, and a panel shows the log's tail.
    arch::boot::platform_devices(kernel_args.rsdp_addr);

    #[cfg(feature = "boot-actuators")]
    arch::boot::interrupt_selftests();

    virtio_console::init(&pci_devices);

    virtio_sound::init(&pci_devices);
    drivers::hda::init(&pci_devices);

    if let Some((gpu_driver, gpu_info)) = virtio_gpu::init(&pci_devices) {
        log!("GPU: using VirtIO");
        // virtio's scanout is only reachable through a virtqueue round trip behind GPU.lock(), which the panic path may not take.
        drivers::panic_console::disable();
        register_gpu(gpu_driver, gpu_info);
    } else if kernel_args.gop_framebuffer != 0 {
        log!("GPU: using UEFI GOP");
        match gop::init(
            kernel_args.gop_framebuffer,
            kernel_args.gop_framebuffer_size,
            kernel_args.gop_width,
            kernel_args.gop_height,
            kernel_args.gop_stride,
            kernel_args.gop_pixel_format,
        ) {
            Some((gpu_driver, gpu_info)) => register_gpu(gpu_driver, gpu_info),
            None => log!("GPU: none this boot, running headless"),
        }
    } else {
        log!("GPU: none found, running headless");
    }

    boot_phase!("devices ready", t_devices);

    // Before userland, so nothing else is reading the input queues.
    #[cfg(feature = "boot-actuators")]
    if actuator::test_input_merge() {
        input_merge_test::run();
    }

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

    // After klogd: its lines are this test image's whole evidence.
    #[cfg(target_arch = "x86_64")]
    arch::i2chid::start(kernel_args.rsdp_addr);

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

