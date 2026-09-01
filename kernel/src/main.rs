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

mod shootdown;
mod sleeplock;
mod smp_roster;
mod sync;
mod id_map;

// No `mod` line below carries an `#[allow(clippy::undocumented_unsafe_blocks)]`.
mod arch;
mod drivers;

#[macro_use]
mod log;
mod actuator;
mod mm;
mod panic;

mod keyboard;
mod mouse;
#[cfg(feature = "boot-actuators")]
mod input_merge_test;
#[cfg(feature = "boot-actuators")]
mod usb_gate;
#[cfg(feature = "boot-actuators")]
mod nvme_gate;
#[cfg(feature = "boot-actuators")]
mod sched_gate;
#[cfg(feature = "boot-actuators")]
mod nmi_gate;
mod block;
mod durability;
mod gpt;
mod page_cache;
mod rollback;
mod file_cache;
#[cfg(feature = "boot-actuators")]
mod leak_selftest;
#[cfg(feature = "boot-actuators")]
mod revoke_selftest;
mod writeback;
mod tmpfs;
mod file_backing;
mod bcachefs_adapter;
mod fat32_adapter;
mod fs_rename;
#[cfg(feature = "boot-actuators")]
mod heartbeat;
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
mod irq_census;
mod irq_ring;
mod trace;
mod time;
mod clock;
mod rtc;

mod completion;
mod iod;
mod object;
mod inbox;
mod pipe;

mod device;
mod net;
mod gpu;
mod user_ptr;
mod vma;

/// Nested generic forces a demangled symbol wider than the console grid,
/// proving `screen_late_panic`'s renderer really wraps.
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

use crate::mm::paging::MmioPolicy;
use alloc::boxed::Box;
use arch::{apic, cpu, idt, pat, percpu, smp, syscall};
use drivers::{acpi, gop, i8042, ioapic, nvme, pci, serial, virtio_console, virtio_gpu, virtio_net, virtio_sound, xhci};
use toyos_abi::boot::{KernelArgs, MemoryMapEntry};

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    cpu::disable_interrupts();

    // Must run first: captures state for a possible second panic, declining if this CPU is already inside one.
    panic::record_panic(info);

    // Reentry guard, checked before any fallible access — else a panic here recurses until the stack meets the heap.
    let depth = panic::depth_slot();
    if depth.fetch_add(1, core::sync::atomic::Ordering::SeqCst) > 0 {
        // UART only: percpu and the log path are what just panicked.
        panic::last_words("PANIC REENTRY: CPU halted", None, info, false);
        // No capture(): the outer panic's snapshot is the one worth showing.
        // render() is safe by construction here: a fault inside the renderer itself would find PAINTING already held, and return without touching a pixel.
        drivers::panic_console::render();
        cpu::halt();
    }

    // Early boot: percpu not ready, just halt (single CPU at this point)
    if !log::PERCPU_READY.load(core::sync::atomic::Ordering::Relaxed) {
        alert!("EARLY PANIC: {}", info);
        // Halts directly instead of via halt_all_cpus: idt::init hasn't run yet, so a renderer fault would triple-fault.
        drivers::panic_console::capture();
        // SAFETY: no other writer can be mid-transmission — IF is clear here and every other CPU is about to halt.
        unsafe { drivers::serial::panic_flush(); }
        // Flush before render: the serial report survives even if render then faults.
        drivers::panic_console::render();
        cpu::halt();
    }

    let prev = percpu::swap_fault_state(percpu::CpuFaultState::Panic);
    if prev != percpu::CpuFaultState::Normal {
        // Escalate: reentry depth is zero here, so this landed on a fatal exception or page fault no handler was inside.
        panic::last_words("DOUBLE PANIC", Some(prev), info, true);
        apic::halt_all_cpus();
    }

    let rbp: u64;
    // SAFETY: register-to-register mov only (nomem, nostack); -Cforce-frame-pointers=yes makes rbp a real frame pointer.
    unsafe { core::arch::asm!("mov {}, rbp", out(reg) rbp, options(nomem, nostack)); }

    arch::idt::exceptions::crash_report(
        &arch::idt::exceptions::CrashInfo::Panic { message: info, rbp }
    );

    // Captures now: recovery below may re-enter a scheduler this panic left locked, so a later drain isn't guaranteed.
    drivers::panic_console::capture();
    // SAFETY: IF is clear on this CPU and every other one halts before anything else can write the port.
    unsafe { drivers::serial::panic_flush(); }

    // Recoverable only when a syscall is what's panicking, or a kthread says its own row's answer.
    let recoverable = sched::kthread::panic_recovers_here().unwrap_or_else(percpu::in_syscall);
    if recoverable {
        depth.store(0, core::sync::atomic::Ordering::SeqCst);
        // Discarded here: a stale capture would blame this panic for the next fatal one.
        drivers::panic_console::discard_capture();
        arch::idt::exceptions::try_recover_from_panic();
    }

    apic::halt_all_cpus();
}

/// Entry point: the bootloader jumps here with `rdi = &KernelArgs`, switches to the kernel's own stack, calls `kernel_main`.
/// # Safety
/// Only the bootloader may call this, fresh from firmware, with `rdi` holding a live [`KernelArgs`].
#[unsafe(naked)]
#[no_mangle]
pub unsafe extern "sysv64" fn _start(_kernel_args: &KernelArgs) -> ! {
    core::arch::naked_asm!(
        "mov rax, [rdi + 16]",  // kernel_memory_addr
        "add rax, [rdi + 32]",  // + kernel_stack_addr
        "add rax, [rdi + 40]",  // + kernel_stack_size
        "movabs rbx, {phys_offset}",
        "add rax, rbx",
        "mov rsp, rax",
        "call {kernel_main}",
        phys_offset = const PHYS_OFFSET,
        kernel_main = sym kernel_main,
    );
}

fn register_gpu(driver: Box<dyn gpu::Gpu>, info: gpu::GpuInfo) {
    gpu::register(driver, info);
}

/// Says where this boot's log can be read, on the last surface still showing it once userland owns the screen.
fn report_log_destination() {
    // Kernel-side because panic_console owns the panel; logd reports which file it opened separately.
    // has_log reflects whether /log mounted, not whether logd could open a file on it.
    let has_log = vfs::lock().has_mount(fat32_adapter::Role::Log.mount());
    // ASCII only: the panel's font renders anything outside 0x20..=0x7E as a dot.
    match (drivers::serial::has_console(), has_log) {
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

unsafe fn kernel_main(kernel_args: &KernelArgs) -> ! {
    // Copied onto the kernel stack: the original lives on the UEFI stack, unreachable once mm::init drops the identity map.
    let kernel_args = *kernel_args;

    let entry_count = kernel_args.memory_map_size as usize / core::mem::size_of::<MemoryMapEntry>();
    let maps = core::slice::from_raw_parts(
        DirectMap::from_phys(kernel_args.memory_map_addr).as_ptr::<MemoryMapEntry>(),
        entry_count,
    );

    // Before serial::init: the screen may be the only surviving channel if serial::init itself faults.
    drivers::panic_console::arm(&kernel_args, maps);

    serial::init();

    // After both channels exist, before the first actuator site.
    // cmdline_len==0 is checked first: an empty bootloader Vec has no backing allocation to point at.
    actuator::init(if kernel_args.cmdline_len == 0 {
        ""
    } else {
        core::str::from_utf8(core::slice::from_raw_parts(
            DirectMap::from_phys(kernel_args.cmdline_addr).as_ptr::<u8>(),
            kernel_args.cmdline_len as usize,
        ))
        .expect("the boot parameter is not UTF-8")
    });

    // Before pat::init, which restores whatever CR0 it found — a firmware CD would ride straight through otherwise.
    arch::control_regs::init_cr0(0);

    // Before panic_console::remap and mm::init: they're the first to map a page selecting the entry this writes.
    pat::init();
    log!("PAT: IA32_PAT={:#018x}, entry {} = {}",
        pat::msr(), pat::WC_ENTRY, pat::entry_name(pat::WC_ENTRY));

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
        "boot: memory map {:#x}+{:#x}, kernel {:#x}+{:#x}, stack {:#x}+{:#x}",
        kernel_args.memory_map_addr, kernel_args.memory_map_size,
        kernel_args.kernel_memory_addr, kernel_args.kernel_memory_size,
        kernel_args.kernel_stack_addr, kernel_args.kernel_stack_size
    );
    log!(
        "boot: initrd {:#x}+{:#x}, kernel elf {:#x}+{:#x}, rsdp {:#x}, boot pml4 {:#x}",
        kernel_args.initrd_addr, kernel_args.initrd_size,
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
        "boot: rtc utc offset {} minutes (known={}), cmdline {:#x}+{}",
        kernel_args.rtc_utc_offset_minutes, kernel_args.rtc_utc_offset_known,
        kernel_args.cmdline_addr, kernel_args.cmdline_len
    );

    let initrd = core::slice::from_raw_parts(
        DirectMap::from_phys(kernel_args.initrd_addr).as_ptr::<u8>(),
        kernel_args.initrd_size as usize,
    );
    let kernel_elf = core::slice::from_raw_parts(
        DirectMap::from_phys(kernel_args.kernel_elf_addr).as_ptr::<u8>(),
        kernel_args.kernel_elf_size as usize,
    );
    let kernel_args = &kernel_args;

    let reserved = [
        mm::Region { start: kernel_args.kernel_memory_addr, end: kernel_args.kernel_memory_addr + kernel_args.kernel_memory_size },
        mm::Region { start: kernel_args.initrd_addr, end: kernel_args.initrd_addr + kernel_args.initrd_size },
        mm::Region { start: kernel_args.kernel_elf_addr, end: kernel_args.kernel_elf_addr + kernel_args.kernel_elf_size },
        mm::Region { start: kernel_args.kernel_stack_addr, end: kernel_args.kernel_stack_addr + kernel_args.kernel_stack_size },
        mm::Region { start: 0x8000, end: 0x9000 }, // AP trampoline page
    ];

    mm::init(maps, &reserved);
    drivers::panic_console::remap();

    // Exception handlers first: a bug in a later phase then diagnoses instead of triple-faulting.
    let madt = acpi::parse_madt(kernel_args.rsdp_addr).expect("ACPI: MADT not found");
    apic::init();
    percpu::init_bsp(apic::id());
    idt::init();
    ioapic::init(&madt);
    idt::enable_interrupts();
    syscall::init();
    symbols::set_kernel_base(kernel_args.kernel_memory_addr);
    if !kernel_elf.is_empty() {
        symbols::load_kernel(kernel_elf, mm::PHYS_OFFSET + kernel_args.kernel_memory_addr);
    }

    // HPET clock — enables profiling for everything from here on
    let hpet_base = acpi::find_hpet_base(kernel_args.rsdp_addr)
        .expect("ACPI: HPET not found");
    clock::init(hpet_base);
    // Century register and time zone both come from ACPI/firmware, not the RTC's own registers.
    let century_reg = match acpi::rtc_century_register(kernel_args.rsdp_addr) {
        Ok(reg) => reg,
        Err(e) => {
            log!("ACPI: the FADT is unreadable ({e:?}), so where the RTC keeps its century is unknown too");
            None
        }
    };
    clock::init_wall(century_reg, kernel_args.rtc_utc_offset());
    trace::enable();
    apic::init_timer();

    boot_phase!("CPU ready", 0);

    let t_storage = clock::nanos_since_boot();

    let ecam_base = acpi::find_ecam_base(kernel_args.rsdp_addr)
        .expect("ACPI: failed to find ECAM base address");
    let ecam = mm::paging::map_mmio(ecam_base, 256 * 32 * 8 * 4096, MmioPolicy::Uncacheable);
    let pci_devices = pci::enumerate(&ecam);
    #[cfg(feature = "boot-actuators")]
    if actuator::pci_cap_selftest() {
        drivers::virtio::cap_selftest();
    }
    // After ACPI is readable and PCI is enumerable, before any driver `init`: each enumerated device needs a context entry before it can DMA.
    // Refuses nothing — a machine with no usable IOMMU boots exactly as one without it.
    iommu::init(kernel_args.rsdp_addr, &pci_devices);
    file_cache::init();
    gpt::init(kernel_args);

    // No controller is a configuration, not a failure — same as a missing xHCI, NIC, or sound device.
    // `None` from open_home means a disk exists but isn't ours; both land on tmpfs.
    let home_volume = match nvme::init(&pci_devices) {
        Some(mut nvme_dev) => {
            // Before page_cache takes the device: only here are blocks still addressed in the device's own numbering.
            let sector_size = nvme_dev.sector_size();
            gpt::probe(&mut nvme_dev, sector_size);
            page_cache::init(Box::new(nvme_dev));
            // Before anything mounts the device: the block the gate reads is one nothing else is touching yet.
            #[cfg(feature = "boot-actuators")]
            if actuator::nvme_spent_budget() {
                nvme_gate::run();
            }
            #[cfg(feature = "boot-actuators")]
            if actuator::nvme_command_silent() {
                nvme_gate::silent_command();
            }
            bcachefs_adapter::open_home()
        }
        None => {
            log!("NVMe: no controller on this machine, storage unavailable");
            None
        }
    };

    boot_phase!("storage ready", t_storage);

    // Under Drain::Inline every record above is already on the wire, so this gate reads the whole boot and then silence.
    #[cfg(feature = "boot-actuators")]
    if actuator::pre_idle_wedge() {
        pre_idle_wedge();
    }

    let t_periph = clock::nanos_since_boot();

    xhci::init(&pci_devices);
    #[cfg(feature = "boot-actuators")]
    if actuator::usb_storage_gate() {
        usb_gate::run();
    }
    // After xhci::init, not beside the NVMe probe: a USB-booted disk doesn't exist until the controller binds it.
    fat32_adapter::probe_boot_disks();
    i8042::init(kernel_args.rsdp_addr);
    acpi::init_power(kernel_args.rsdp_addr);

    boot_phase!("peripherals ready", t_periph);

    let t_subsys = clock::nanos_since_boot();

    smp::boot_aps(&madt, kernel_args.boot_pml4_addr);
    vfs::init();
    process::init();
    scheduler::init();
    // Task-less half of the operation-nesting gate: this boot phase has no current task, so it establishes into the per-CPU slot.
    #[cfg(feature = "boot-actuators")]
    if actuator::sched_operation_nesting() {
        sched_gate::run("boot");
    }
    pipe::init();
    inbox::init();


    // (base, len) is named once here; every file backing under this mount holds the same pair, checkable against the initrd's end.
    assert!(!initrd.is_empty(), "No initrd provided");
    // SAFETY: initrd is bootloader-reserved memory, never freed or written, valid for the image's whole lifetime.
    let initrd_image = unsafe { bcachefs::SliceBlockIO::new(initrd.as_ptr(), initrd.len()) };
    let initrd_fs = bcachefs_adapter::mount_initrd(initrd_image);
    vfs::lock().set_root(Box::new(bcachefs_adapter::ReadOnlyBcacheFsAdapter::new(initrd_fs, initrd_image)));

    // tmpfs when the NVMe device isn't ours to write: persistence is the only difference, so the earlier refusal doesn't cascade.
    use vfs::UserAccess;
    match home_volume {
        Some(fs) => vfs::lock().mount("home", Box::new(bcachefs_adapter::BcacheFsAdapter::new(fs)), UserAccess::ReadWrite),
        None => {
            log!("storage: /home is a tmpfs — it will not survive a reboot");
            vfs::lock().mount("home", Box::new(crate::tmpfs::TmpFs::new()), UserAccess::ReadWrite)
        }
    }
    vfs::lock().mount("tmp", Box::new(crate::tmpfs::TmpFs::new()), UserAccess::ReadWrite);

    // Named by role, not type: both partitions are FAT32 and neither is selected for being FAT32 — a missing one just has no mount.
    use fat32_adapter::Role;
    // /boot is KernelOnly: a writable /boot lets a process brick the machine — esp_files replayed exactly that attack.
    // The filesystem sits outside the capability model by ruling, so no handle is owed for /boot.
    // /log is ReadWrite on purpose: it's an ordinary userland file logd owns, and the worst a process can do is cost the diagnostic.
    match fat32_adapter::mount(Role::Boot) {
        Some(fs) => vfs::lock().mount(Role::Boot.mount(), Box::new(fs), UserAccess::KernelOnly),
        None => log!("boot-volume: not mounted; the kernel has no /boot this boot"),
    }
    match fat32_adapter::mount(Role::Log) {
        Some(fs) => {
            vfs::lock().mount(Role::Log.mount(), Box::new(fs), UserAccess::ReadWrite);
        }
        // No fallback onto /boot: with no log partition the log stays in the in-memory shards, still reachable via screen and console.
        None => log!("log-volume: not mounted; this boot's kernel log stays in memory"),
    }

    // Fixed kernel strings well under MAX_PATH: a refusal here is a kernel bug, so this fails fast instead of returning an error.
    vfs::lock().create_dir("/home/root").expect("boot: /home/root exceeds MAX_PATH");
    vfs::lock().create_dir("/home/root/.config").expect("boot: /home/root/.config exceeds MAX_PATH");

    boot_phase!("subsystems ready", t_subsys);

    // After the mounts above: the FAT reopen control drives `/log`.
    #[cfg(feature = "boot-actuators")]
    if actuator::leak_rollback_selftest() {
        leak_selftest::run();
    }
    #[cfg(feature = "boot-actuators")]
    if actuator::revoked_backing_selftest() {
        revoke_selftest::run();
    }
    #[cfg(feature = "boot-actuators")]
    if actuator::pc_unbind_selftest() {
        page_cache::unbind_selftest();
    }

    let t_devices = clock::nanos_since_boot();

    // Runs once for the machine: it touches no device, so per-driver repetition would say the same thing four times.
    #[cfg(feature = "boot-actuators")]
    if actuator::virtio_used_selftest() {
        drivers::virtio::used_selftest();
    }

    // Needs interrupts on and the timer already ticking: its last assertion is that the interrupt after the spurious one arrives.
    #[cfg(feature = "boot-actuators")]
    if actuator::lapic_spurious_selftest() {
        arch::idt::spurious::selftest();
    }

    #[cfg(feature = "boot-actuators")]
    if actuator::unclaimed_vector_selftest() {
        arch::idt::unclaimed::selftest();
    }

    virtio_console::init(&pci_devices);
    virtio_net::init(&pci_devices);

    virtio_sound::init(&pci_devices);
    drivers::hda::init(&pci_devices);

    if let Some((gpu_driver, gpu_info)) = virtio_gpu::init(&pci_devices) {
        log!("GPU: using VirtIO");
        // virtio's scanout is only reachable through a virtqueue round trip behind GPU.lock(), which the panic path may not take.
        drivers::panic_console::disable();
        register_gpu(gpu_driver, gpu_info);
    } else if kernel_args.gop_framebuffer != 0 {
        log!("GPU: using UEFI GOP");
        let (gpu_driver, gpu_info) = gop::init(
            kernel_args.gop_framebuffer,
            kernel_args.gop_framebuffer_size,
            kernel_args.gop_width,
            kernel_args.gop_height,
            kernel_args.gop_stride,
            kernel_args.gop_pixel_format,
        );
        register_gpu(gpu_driver, gpu_info);
    } else {
        log!("GPU: none found, running headless");
    }

    boot_phase!("devices ready", t_devices);

    // Before userland, so nothing else is reading the input queues.
    #[cfg(feature = "boot-actuators")]
    if actuator::test_input_merge() {
        input_merge_test::run();
    }

    // init reads /etc/system.manifest itself; the boot config never names the program it starts.
    let pid = process::spawn_init();
    log!("spawned {} pid={pid}", process::INIT_PATH);

    // Here and not beside the other controls: it needs a process the table answers for.
    #[cfg(feature = "boot-actuators")]
    if actuator::process_reopen_selftest() {
        object::process::reopen_selftest(pid);
    }

    report_log_destination();
    boot_phase!("complete", 0);

    // No current task here, so the handler's recovery predicate fails — the one panic no userland process can produce.
    #[cfg(feature = "boot-actuators")]
    if actuator::test_late_panic() {
        late_panic::Nest::<late_panic::Nest<late_panic::Nest<late_panic::Nest<
            late_panic::Nest<late_panic::Nest<late_panic::Nest<late_panic::Nest<
            late_panic::Nest<late_panic::Nest<()>>>>>>>>>>::on_screen_console_check();
    }

    // Same no-current-task window as above: blame is Kernel, so fatal_exception halts the machine.
    if actuator::test_kernel_fault() {
        // SAFETY: ud2 reads and writes nothing (nomem, nostack) and raises #UD, caught by the already-installed IDT.
        unsafe { core::arch::asm!("ud2", options(nomem, nostack)) };
    }

    // Last thing before enter_idle_loop: nothing can run before it, and a klogd spawned earlier would idle through phases 5-7 with no drainer.
    log::console::start();
    // After klogd so their own spawn logs have a drainer.
    drivers::xhci::usbd::start();
    iod::start();

    smp::set_ready();
    crate::scheduler::enter_idle_loop();
}

/// Wedges the machine: interrupts off then spin, with no timer, scheduler, or klogd left to drain anything logged after this.
#[cfg(feature = "boot-actuators")]
fn pre_idle_wedge() -> ! {
    log!("pre-idle-wedge: the boot stops here, and this line is the last thing this machine says");
    cpu::disable_interrupts();
    loop {
        core::hint::spin_loop();
    }
}
