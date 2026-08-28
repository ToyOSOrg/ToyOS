use core::arch::global_asm;
use core::mem::size_of;
use core::sync::atomic::{AtomicU32, Ordering};

use alloc::alloc::{alloc_zeroed, Layout};

use crate::arch::{apic, percpu, syscall};
use crate::clock;
use crate::drivers::acpi::MadtInfo;
use crate::smp_roster::Roster;
use crate::time::{Budget, Delay, Duration};
use crate::{log, process};

const TRAMPOLINE_PAGE: u64 = 0x8000;
const TRAMPOLINE_VECTOR: u8 = 0x08;
const AP_STACK_SIZE: usize = 64 * 1024;
const DATA_OFFSET: usize = 0xF00;

/// The token the latest-launched AP echoed at `ap_entry`, not a flag, so a stale
/// AP cannot be read as this one; `0` means none has.
static AP_STARTED: AtomicU32 = AtomicU32::new(0);

static ROSTER: Roster = Roster::new();

const _: () = assert!(crate::smp_roster::MAX_CPUS == crate::scheduler::MAX_CPUS);

pub fn cpu_count() -> u32 {
    ROSTER.count()
}

/// LAPIC id of `cpu_id`; panics if `cpu_id` is not online.
pub fn apic_id_for(cpu_id: u32) -> u32 {
    assert!(cpu_id < cpu_count(), "apic_id_for: cpu {cpu_id} not online");
    ROSTER.apic_id(cpu_id)
}

/// True once a shootdown must wait for siblings; the word the APs are released by.
pub(crate) fn answering() -> bool {
    ROSTER.answering()
}

/// Release the APs into the scheduler and, by the same store, start answering their shootdowns.
pub fn set_ready() {
    ROSTER.release();
}

// Field offsets are hardcoded in the global_asm! trampoline below; the static assertion at the bottom checks the match.

#[derive(Clone, Copy)]
#[repr(C, packed)]
struct DescriptorTablePointer {
    limit: u16,
    base: u64,
}

#[derive(Clone, Copy)]
#[repr(C, packed)]
struct DescriptorTablePointer32 {
    limit: u16,
    base: u32,
}

#[derive(Clone, Copy)]
#[repr(C, packed)]
struct FarPointer {
    offset: u32,
    selector: u16,
}

#[derive(Clone, Copy)]
#[repr(C, packed)]
struct TrampolineData {
    cr3: u64,                                // +0x00
    stack_top: u64,                          // +0x08
    entry: u64,                              // +0x10
    kernel_gdt: DescriptorTablePointer,      // +0x18 (10 bytes)
    _pad1: [u8; 6],                          // +0x22
    kernel_idt: DescriptorTablePointer,      // +0x28 (10 bytes)
    _pad2: [u8; 6],                          // +0x32
    temp_gdt_ptr: DescriptorTablePointer32,  // +0x38 (6 bytes)
    _pad3: [u8; 2],                          // +0x3E
    temp_gdt: [u64; 4],                      // +0x40 (32 bytes)
    pm32_far: FarPointer,                    // +0x60 (6 bytes)
    _pad4: [u8; 2],                          // +0x66
    lm64_far: FarPointer,                    // +0x68 (6 bytes)
    _pad5: [u8; 2],                          // +0x6E
    cs_reload_addr: u64,                     // +0x70
    percpu_ptr: u64,                         // +0x78
}

const _: () = assert!(size_of::<TrampolineData>() == 0x80);



extern "C" {
    static _trampoline_start: u8;
    static _trampoline_end: u8;
    static _ap_pm32: u8;
    static _ap_lm64: u8;
    static _ap_cs_reload: u8;
}

/// Address of an assembly label via a `rip`-relative `lea`; reads no memory, so this needs no `unsafe` contract from callers.
// A macro, not a function: `sym` needs the label as a compile-time token.
// Inline asm, not `extern "C" { static }`: the PIE linker's GOT stubs for these labels resolve in the wrong order.
macro_rules! asm_label_addr {
    ($label:ident) => {{
        let addr: usize;
        // SAFETY: `lea` with `nomem` reads and writes no memory.
        unsafe {
            core::arch::asm!(
                "lea {}, [rip + {}]",
                out(reg) addr,
                sym $label,
                options(nostack, nomem),
            );
        }
        addr as *const u8
    }};
}

/// Copies the trampoline blob to physical page 0x8000 via the direct map; there is no identity map this early.
fn copy_trampoline() {
    let start = asm_label_addr!(_trampoline_start);
    let end = asm_label_addr!(_trampoline_end);
    let size = end as usize - start as usize;
    assert!(size <= DATA_OFFSET, "trampoline code exceeds data block");
    let dest = crate::DirectMap::from_phys(TRAMPOLINE_PAGE).as_mut_ptr::<u8>();
    // SAFETY: `start..end` is this kernel's own `.text`; `dest` is physical page 0x8000, reserved for this and never allocator-owned; the asserted `size <= DATA_OFFSET` keeps the copy clear of the `TrampolineData` block and inside the page; the two ranges are kernel text and low physical memory, so they cannot overlap.
    unsafe {
        core::ptr::copy_nonoverlapping(start, dest, size);
    }
}

/// Compute the runtime physical address of a trampoline label.
fn label_addr(label: *const u8) -> u32 {
    let base = asm_label_addr!(_trampoline_start) as usize;
    0x8000u32 + (label as usize - base) as u32
}

/// Build the TrampolineData struct with all global (non-per-AP) fields filled.
fn build_trampoline_data() -> TrampolineData {
    let pm32_addr = label_addr(asm_label_addr!(_ap_pm32));
    let lm64_addr = label_addr(asm_label_addr!(_ap_lm64));
    let cs_reload_addr = label_addr(asm_label_addr!(_ap_cs_reload)) as u64;

    let mut kernel_gdt = DescriptorTablePointer { limit: 0, base: 0 };
    let mut kernel_idt = DescriptorTablePointer { limit: 0, base: 0 };
    // SAFETY: both write ten bytes to a `&mut DescriptorTablePointer` (`repr(C, packed)` u16+u64), matching that shape and size.
    unsafe {
        core::arch::asm!("sgdt [{}]", in(reg) &mut kernel_gdt, options(nostack));
        core::arch::asm!("sidt [{}]", in(reg) &mut kernel_idt, options(nostack));
    }

    let data_base = (TRAMPOLINE_PAGE + DATA_OFFSET as u64) as u32;

    TrampolineData {
        cr3: 0, // filled by boot_aps with the boot PML4 (has identity + high-half)
        stack_top: 0, // filled per-AP
        entry: 0,     // filled per-AP
        kernel_gdt,
        _pad1: [0; 6],
        kernel_idt,
        _pad2: [0; 6],
        temp_gdt_ptr: DescriptorTablePointer32 {
            limit: 4 * 8 - 1,
            base: data_base + 0x40, // points to temp_gdt field
        },
        _pad3: [0; 2],
        temp_gdt: [
            0x0000_0000_0000_0000, // null
            0x00CF_9A00_0000_FFFF, // code32
            0x00CF_9200_0000_FFFF, // data
            0x00AF_9A00_0000_FFFF, // code64
        ],
        pm32_far: FarPointer { offset: pm32_addr, selector: 0x08 },
        _pad4: [0; 2],
        lm64_far: FarPointer { offset: lm64_addr, selector: 0x18 },
        _pad5: [0; 2],
        cs_reload_addr,
        percpu_ptr: 0, // filled per-AP
    }
}

/// Boot all Application Processors found in the MADT, using `boot_cr3` — the bootloader's identity+high-half PML4 — until each AP switches to the kernel PML4 in `ap_entry`.
pub fn boot_aps(madt: &MadtInfo, boot_cr3: u64) {
    let bsp_id = apic::id();
    ROSTER.set_bsp(bsp_id);
    copy_trampoline();

    let mut data = build_trampoline_data();
    data.cr3 = boot_cr3;
    let target = crate::DirectMap::from_phys(TRAMPOLINE_PAGE + DATA_OFFSET as u64).as_mut_ptr::<TrampolineData>();

    for &ap_id in &madt.apic_ids {
        if ap_id == bsp_id { continue; }

        // Refused at MAX_CPUS, bounding a firmware that over-reports CPUs.
        let Some(attempt) = ROSTER.begin_attempt() else {
            log!("SMP: roster full at {} CPUs; ignoring further MADT entries", cpu_count());
            break;
        };

        let stack_layout = Layout::from_size_align(AP_STACK_SIZE, 4096).unwrap();
        // SAFETY: `AP_STACK_SIZE` is non-zero and 4096 is a power of two, `alloc_zeroed`'s whole contract.
        // Never freed: the block becomes an AP's `rsp`, so no owning handle can hold it.
        let stack_base = unsafe { alloc_zeroed(stack_layout) };
        assert!(!stack_base.is_null(), "SMP: failed to allocate AP stack");

        let ap_percpu = percpu::alloc_ap(attempt.id(), attempt.token());

        data.stack_top = stack_base as u64 + AP_STACK_SIZE as u64;
        data.entry = ap_entry as *const () as u64;
        data.percpu_ptr = ap_percpu as u64;
        // SAFETY: `target` is reserved physical memory written only here; unaligned because `TrampolineData` is `repr(C, packed)`; this AP has not been sent its SIPI yet, and the loop reaches a second write only after the previous AP committed — which happens in `ap_entry` past every trampoline read — so no CPU is reading it; a failed AP breaks the loop below instead of reaching another write.
        unsafe { core::ptr::write_unaligned(target, data); }

        AP_STARTED.store(0, Ordering::Release);

        // Both delays are spent, not waited on: nothing is polled across either.
        const AFTER_INIT: Delay =
            Delay::from_spec(Duration::from_millis(10), "SDM §8.4.4.1, after the INIT IPI");
        /// SDM §8.4.4.1 asks 200us here; a millisecond gives room and is paid once per AP.
        const BETWEEN_SIPIS: Delay =
            Delay::from_spec(Duration::from_millis(1), "SDM §8.4.4.1, between the two SIPIs");
        if !skip_startup(attempt.id()) {
            apic::send_init(ap_id);
            delay(AFTER_INIT);

            apic::send_sipi(ap_id, TRAMPOLINE_VECTOR);
            delay(BETWEEN_SIPIS);

            if AP_STARTED.load(Ordering::Acquire) != attempt.token() {
                apic::send_sipi(ap_id, TRAMPOLINE_VECTOR);
            }
        }

        // Budget, not Tripwire or panic: a slow AP degrades the machine rather than crashing it, and a panic here would be a behaviour change C1's gate forbids.
        const AP_START: Budget = Budget::of(
            Duration::from_millis(100),
            "the machine boots with the CPUs that came up before the first that did not",
        );
        let deadline = clock::nanos_since_boot() + AP_START.nanos();
        while AP_STARTED.load(Ordering::Acquire) != attempt.token() {
            if clock::nanos_since_boot() >= deadline { break; }
            core::hint::spin_loop();
        }

        // Commit only on this attempt's own token, so `0..cpu_count()` stays dense.
        if AP_STARTED.load(Ordering::Acquire) == attempt.token() {
            ROSTER.commit(attempt, ap_id);
            log!("SMP: AP cpu{} lapic={} online", attempt.id(), ap_id);
        } else {
            // Neither the id nor the trampoline is reused after a failure: stop here.
            log!("SMP: AP cpu{} lapic={} failed to start! remaining APs stay offline", attempt.id(), ap_id);
            break;
        }
    }
}

/// The actuator staging a non-last AP that never starts; `false` without `boot-actuators`.
fn skip_startup(id: u32) -> bool {
    crate::actuator::smp_skip_ap() && id == 2
}

extern "C" fn ap_entry() -> ! {
    // Must run before `pat::init`, which restores the CR0 this call sets.
    crate::arch::control_regs::init_cr0(percpu::cpu_id());

    // Must run before this CPU touches the framebuffer, which needs write-combining mapped first.
    crate::arch::pat::init();

    crate::mm::paging::load_kernel_flush();

    // GS base was set by the trampoline; finish percpu init (GDT, CR4).
    percpu::init_ap(percpu::percpu_ptr());
    syscall::init();
    // Calibration is a one-time BSP measurement; nothing left for an AP to do here.
    apic::init_ap();

    // Echo this attempt's token, so the BSP counts this AP for its own attempt.
    AP_STARTED.store(percpu::ap_token(), Ordering::Release);

    while !ROSTER.released() {
        core::hint::spin_loop();
    }

    // Only a committed CPU may join: an uncommitted AP has no scheduler slot and
    // no shootdown targets it, so it parks. The acquire above makes the count visible.
    let me = percpu::cpu_id();
    if me >= cpu_count() {
        log!("CPU {me}: bring-up did not commit; parking");
        park_ap();
    }

    // A parked AP could not take a shootdown IPI, so this flush stands in for the ones missed while spinning.
    // Must run before touching anything not self-mapped: the acquire on `released` makes the BSP's mappings visible, and this flush discards what the spin cached over them.
    crate::arch::tlb::join();

    log!("CPU {me}: joining scheduler");
    process::ap_idle();
}

/// Spend a [`Delay`] by spinning; boot-only, there is no scheduler to park against.
fn delay(span: Delay) {
    let start = clock::nanos_since_boot();
    while clock::nanos_since_boot() - start < span.nanos() {}
}

/// An uncommitted AP halts for the life of the machine.
fn park_ap() -> ! {
    loop {
        // SAFETY: `cli; hlt` on this CPU only; it takes no lock and touches no shared state.
        unsafe { core::arch::asm!("cli; hlt", options(nomem, nostack)) };
    }
}

// Real mode → protected mode → long mode → Rust entry, copied to 0x8000 at runtime; addresses below are offsets into TrampolineData at 0x8F00.
global_asm!(
    ".global _trampoline_start",
    ".global _trampoline_end",
    ".global _ap_pm32",
    ".global _ap_lm64",
    ".global _ap_cs_reload",
    "_trampoline_start:",

    ".code16",
    "cli",
    "xor ax, ax",
    "mov ds, ax",
    "mov es, ax",
    "mov ss, ax",

    // Load temp GDT descriptor from TrampolineData.temp_gdt_ptr (+0x38)
    "lgdt [0x8F38]",

    // Enable Protected Mode (CR0.PE)
    "mov eax, cr0",
    "or al, 1",
    "mov cr0, eax",

    // Far jump to PM32 via TrampolineData.pm32_far (+0x60)
    ".byte 0x66, 0xFF, 0x2E",  // data32 jmp far [disp16]
    ".word 0x8F60",

    ".code32",
    "_ap_pm32:",
    "mov ax, 0x10",
    "mov ds, ax",
    "mov es, ax",
    "mov ss, ax",

    // Enable PAE (CR4.PAE)
    "mov eax, cr4",
    "or eax, 0x20",
    "mov cr4, eax",

    // Load PML4 from TrampolineData.cr3 (+0x00)
    "mov eax, [0x8F00]",
    "mov cr3, eax",

    // Enable Long Mode (IA32_EFER.LME)
    "mov ecx, 0xC0000080",
    "rdmsr",
    "or eax, 0x100",
    "wrmsr",

    // Enable Paging (CR0.PG)
    "mov eax, cr0",
    "or eax, 0x80000000",
    "mov cr0, eax",

    // Far jump to LM64 via TrampolineData.lm64_far (+0x68)
    ".byte 0xFF, 0x2D",  // jmp far [disp32]
    ".long 0x8F68",

    ".code64",
    "_ap_lm64:",

    // Load TrampolineData base and set up stack
    "mov edi, 0x8F00",
    "mov rsp, [rdi + 0x08]",   // TrampolineData.stack_top

    // Load kernel GDT and reload CS via retfq
    "lgdt [rdi + 0x18]",       // TrampolineData.kernel_gdt
    "push 0x08",
    "push qword ptr [rdi + 0x70]", // TrampolineData.cs_reload_addr
    ".byte 0x48, 0xCB",        // REX.W RETF

    "_ap_cs_reload:",
    "mov ax, 0x10",
    "mov ds, ax",
    "mov es, ax",
    "mov fs, ax",
    "mov gs, ax",
    "mov ss, ax",

    // Set GS base to percpu pointer (IA32_GS_BASE MSR 0xC0000101)
    // Must happen before IDT load so page fault handlers can access percpu.
    "mov edi, 0x8F00",
    "mov rax, [rdi + 0x78]",  // TrampolineData.percpu_ptr
    "mov rdx, rax",
    "shr rdx, 32",
    "mov ecx, 0xC0000101",    // IA32_GS_BASE
    "wrmsr",

    // Load kernel IDT (safe now — percpu/GS is set up)
    "mov edi, 0x8F00",
    "lidt [rdi + 0x28]",       // TrampolineData.kernel_idt

    // Call Rust entry
    "call qword ptr [rdi + 0x10]", // TrampolineData.entry
    "2: hlt",
    "jmp 2b",

    "_trampoline_end:",
    ".code64",
);
