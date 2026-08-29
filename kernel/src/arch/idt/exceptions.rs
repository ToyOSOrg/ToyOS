use crate::arch::{apic, cpu, syscall, percpu};
use crate::arch::percpu::CpuFaultState;
use crate::{alert, log, mm, process, scheduler, symbols};

use toyos_userbound::{blame, Blame, Faulted, Ring};

use super::{Vector, TrapFrame, PF_PRESENT, PF_WRITE, PF_INSTRUCTION_FETCH};

/// Walk RBP chain for kernel backtrace with symbol resolution.
pub(crate) fn kernel_backtrace(start_rbp: u64, max_frames: usize) {
    let mut rbp = start_rbp;
    for _ in 0..max_frames {
        if rbp == 0 || !rbp.is_multiple_of(8) || !mm::is_kernel_addr(rbp) { break; }
        // SAFETY: `rbp` is checked non-zero, 8-aligned and a kernel address, so
        // both reads land in the direct map, mapped for the life of the machine.
        //
        // Not `read_volatile` like `safe_read_kernel`, whose double-fault path
        // reads memory another CPU may still be writing: this walks the
        // faulting thread's own frame chain from its handler.
        let saved_rbp = unsafe { *(rbp as *const u64) };
        // SAFETY: same as above, for the return address one word up.
        let return_addr = unsafe { *((rbp + 8) as *const u64) };
        if return_addr == 0 || !mm::is_kernel_addr(return_addr) { break; }
        symbols::resolve_kernel_return(return_addr);
        rbp = saved_rbp;
    }
}

/// Walk RBP chain for user backtrace through page tables. Takes no pid: this
/// always backtraces the process running on this CPU.
fn user_backtrace(start_rbp: u64, pml4: *const u64, max_frames: usize) {
    let mut rbp = start_rbp;
    for _ in 0..max_frames {
        if rbp == 0 || !rbp.is_multiple_of(8) { break; }
        let Some(saved_rbp) = safe_read_u64(rbp, pml4) else { break };
        let Some(return_addr) = safe_read_u64(rbp + 8, pml4) else { break };
        if return_addr == 0 { break; }
        process::resolve_user_symbol_return(return_addr).log_bare(return_addr);
        rbp = saved_rbp;
    }
}

/// Walk RBP chain using safe kernel reads only (for double fault handler on IST stack).
fn kernel_backtrace_safe(start_rbp: u64, max_frames: usize) {
    let mut rbp = start_rbp;
    for _ in 0..max_frames {
        let Some(saved_rbp) = safe_read_kernel(rbp) else { break };
        let Some(return_addr) = safe_read_kernel(rbp + 8) else { break };
        if return_addr == 0 { break; }
        symbols::resolve_kernel_return(return_addr);
        rbp = saved_rbp;
    }
}


/// Safe kernel memory read. Only reads kernel direct-map addresses.
fn safe_read_kernel(addr: u64) -> Option<u64> {
    if !addr.is_multiple_of(8) || !mm::is_kernel_addr(addr) {
        return None;
    }
    // SAFETY: `addr` is 8-aligned and a kernel address, checked just above, so
    // the read is inside the direct map. `read_volatile`: this runs on the
    // crash path and another CPU may still be writing the memory.
    Some(unsafe { core::ptr::read_volatile(addr as *const u64) })
}

/// Reads a u64; for a user address, walks page tables by hand to avoid
/// demand-paging faults inside an exception handler.
fn safe_read_u64(addr: u64, user_pml4: *const u64) -> Option<u64> {
    if !addr.is_multiple_of(8) || addr == 0 {
        return None;
    }
    if !user_pml4.is_null() {
        let pml4_idx = ((addr >> 39) & 0x1FF) as usize;
        let pdpt_idx = ((addr >> 30) & 0x1FF) as usize;
        let pd_idx = ((addr >> 21) & 0x1FF) as usize;
        // SAFETY: each read is guarded by the present bit of the entry before
        // it; `user_pml4` is a direct-map pointer to the live PML4 from `CR3`;
        // every index is masked to nine bits, so `add` stays inside the
        // 512-entry table, and each next-level pointer and the final
        // `page_phys + offset` stay inside the direct map by the same walk.
        //
        // Hand-rolled instead of `mm::paging`: a faulted CPU may not take the
        // address space lock and may not demand-page.
        //
        // Not `read_volatile` either: see `kernel_backtrace`.
        let pml4e = unsafe { *user_pml4.add(pml4_idx) };
        if pml4e & 1 == 0 { return None; }
        let pdpt = crate::DirectMap::from_phys(pml4e & 0x000F_FFFF_FFFF_F000).as_ptr::<u64>();
        // SAFETY: the walk's argument, one level down.
        let pdpte = unsafe { *pdpt.add(pdpt_idx) };
        if pdpte & 1 == 0 { return None; }
        let pd = crate::DirectMap::from_phys(pdpte & 0x000F_FFFF_FFFF_F000).as_ptr::<u64>();
        // SAFETY: the walk's argument, one level down again.
        let pde = unsafe { *pd.add(pd_idx) };
        if pde & 1 == 0 { return None; }
        let page_phys = pde & 0x000F_FFFF_FFE0_0000;
        let offset = addr & (mm::PAGE_2M - 1);
        // SAFETY: the walk's argument — a direct-map read of a byte inside the
        // present 2 MiB leaf the three entries above resolved.
        Some(unsafe { *crate::DirectMap::from_phys(page_phys + offset).as_ptr::<u64>() })
    } else if mm::is_kernel_addr(addr) {
        // SAFETY: `addr` is 8-aligned (checked at the top) and a kernel address
        // (checked in this arm), so it is inside the direct map.
        Some(unsafe { *(addr as *const u64) })
    } else {
        None
    }
}

pub(crate) struct ExceptionContext<'a> {
    frame: &'a TrapFrame,
    cr2: u64,
}

impl ExceptionContext<'_> {
    fn vector(&self) -> Vector {
        Vector::from_raw(self.frame.vector)
    }

    fn ring(&self) -> Ring {
        Ring::of_cs(self.frame.cs)
    }

    /// CR2 is meaningful on a #PF and stale on every other vector.
    fn faulted(&self) -> Faulted {
        if self.vector() == Vector::PageFault {
            Faulted::Address(self.cr2)
        } else {
            Faulted::Nothing
        }
    }

    /// Whose fault it was. See `toyos_userbound::fault`.
    fn blame(&self) -> Blame {
        blame(self.ring(), self.frame.rip, self.faulted(), percpu::current_tid().is_some())
    }
}

// DESIGN RULE: crash_report and everything it calls must stay panic-free — no
// unwrap/expect/index, no allocation, no blocking lock; try_lock only. log!()
// and symbol resolution are pre-verified panic-free and lock-free, so calling
// them here does not itself break the rule.

/// Name of a vector, shared by the crash report and `panic::record_fault` so
/// a DOUBLE PANIC names the fault it landed on in the same words.
fn vector_name(vector: Vector) -> &'static str {
    match vector {
        Vector::DivideError => "divide error",
        Vector::Debug => "debug",
        Vector::Breakpoint => "breakpoint",
        Vector::Overflow => "overflow",
        Vector::BoundRange => "bound range exceeded",
        Vector::InvalidOpcode => "invalid opcode",
        Vector::DeviceNotAvailable => "device not available",
        Vector::DoubleFault => "double fault",
        Vector::InvalidTss => "invalid TSS",
        Vector::SegmentNotPresent => "segment not present",
        Vector::StackSegment => "stack fault",
        Vector::GeneralProtection => "general protection fault",
        Vector::PageFault => "page fault",
        Vector::X87FloatingPoint => "x87 floating-point exception",
        Vector::AlignmentCheck => "alignment check",
        Vector::MachineCheck => "machine check",
        Vector::SimdFloatingPoint => "SIMD floating-point exception",
        Vector::Virtualization => "virtualization exception",
        Vector::ControlProtection => "control protection",
        // Vectors with a `direct` gate never reach this report: they skip
        // `trap_dispatch`.
        _ => "exception",
    }
}

/// Source of a crash — either a hardware exception or a Rust panic.
pub(crate) enum CrashInfo<'a> {
    Exception(&'a ExceptionContext<'a>),
    Panic { message: &'a core::panic::PanicInfo<'a>, rbp: u64 },
}

/// Print full crash diagnostics. Used by both fatal_exception and the panic handler.
pub(crate) fn crash_report(info: &CrashInfo) {
    match info {
        CrashInfo::Exception(ctx) => crash_report_exception(ctx),
        CrashInfo::Panic { message, rbp } => crash_report_panic(message, *rbp),
    }
}

fn crash_report_exception(ctx: &ExceptionContext) {
    // `theirs` (who is blamed) and `ring3` (which report format) are separate
    // questions: a syscall fault is the process's fault even though the frame
    // is Ring 0, with a kernel `rip` and a kernel-stack `rbp`.
    let theirs = ctx.blame() != Blame::Kernel;
    let ring3 = ctx.ring().is_user();
    let tid = percpu::current_tid().unwrap_or(crate::process::Tid(0));
    let pid = percpu::current_pid();
    let pml4 = if ring3 { crate::DirectMap::from_phys(crate::mm::paging::Cr3::current().phys()).as_ptr::<u64>() } else { core::ptr::null() };

    let (pf_action, pf_cause) = if ctx.vector() == Vector::PageFault {
        let action = if ctx.frame.error_code & PF_INSTRUCTION_FETCH != 0 { "execute" }
            else if ctx.frame.error_code & PF_WRITE != 0 { "write" }
            else { "read" };
        let cause = if ctx.frame.error_code & PF_PRESENT != 0 { "protection violation" }
            else { "unmapped address" };
        (action, cause)
    } else {
        ("", "")
    };

    let name = vector_name(ctx.vector());

    if theirs {
        match ctx.vector() {
            Vector::PageFault => log!("SEGFAULT tid={}: {} {} at {:#x}", tid, pf_action, pf_cause, ctx.cr2),
            Vector::InvalidOpcode => log!("SIGILL tid={}: illegal instruction", tid),
            Vector::DivideError | Vector::X87FloatingPoint | Vector::SimdFloatingPoint => {
                log!("SIGFPE tid={}: {}", tid, name)
            }
            Vector::GeneralProtection | Vector::StackSegment | Vector::AlignmentCheck => {
                log!("SIGBUS tid={}: {} (error_code={:#x})", tid, name, ctx.frame.error_code)
            }
            _ => log!("FATAL tid={}: {}", tid, name),
        }
    } else {
        match ctx.vector() {
            Vector::PageFault => log!("KERNEL PANIC: {} {} at {:#x}", pf_action, pf_cause, ctx.cr2),
            _ => log!("KERNEL PANIC: {} (error_code={:#x})", name, ctx.frame.error_code),
        }
    }

    log!("  rip:");
    if ring3 {
        if pid.is_some() {
            process::resolve_user_symbol(ctx.frame.rip).log_bare(ctx.frame.rip);
        } else {
            log!("    {:#x}", ctx.frame.rip);
        }
    } else {
        symbols::resolve_kernel(ctx.frame.rip);
    }

    if ctx.vector() == Vector::PageFault {
        crate::mm::paging::debug_page_walk(ctx.cr2);
    }

    log!("  Registers:");
    log!("    rax={:#018x}  rbx={:#018x}", ctx.frame.rax, ctx.frame.rbx);
    log!("    rcx={:#018x}  rdx={:#018x}", ctx.frame.rcx, ctx.frame.rdx);
    log!("    rsi={:#018x}  rdi={:#018x}", ctx.frame.rsi, ctx.frame.rdi);
    log!("    rbp={:#018x}  rsp={:#018x}", ctx.frame.rbp, ctx.frame.rsp);
    log!("     r8={:#018x}   r9={:#018x}", ctx.frame.r8, ctx.frame.r9);
    log!("    r10={:#018x}  r11={:#018x}", ctx.frame.r10, ctx.frame.r11);
    log!("    r12={:#018x}  r13={:#018x}", ctx.frame.r12, ctx.frame.r13);
    log!("    r14={:#018x}  r15={:#018x}", ctx.frame.r14, ctx.frame.r15);
    // A #GP error code is a selector, meaningless without the segments it ran
    // with.
    log!("    cs={:#06x}  ss={:#06x}  rflags={:#018x}",
        ctx.frame.cs, ctx.frame.ss, ctx.frame.rflags);

    // Ahead of both backtraces: a crash report can die mid-print, and this is
    // the part that decides between the two readings of a recursive fault.
    //
    // Only for kernel faults — a Ring 3 segfault says nothing about which CPU
    // is on which kernel stack, and would bury the report about the process.
    if !theirs {
        crate::hw::report_contexts(ctx.frame.rsp, None);
    }

    log!("  Backtrace:");
    if ring3 {
        if pid.is_some() {
            user_backtrace(ctx.frame.rbp, pml4, 32);
        }
    } else {
        kernel_backtrace(ctx.frame.rbp, 32);

        // `Syscall:` is where the thread called in from, not where it faulted.
        // Printed only inside that thread's own syscall: a stale `syscall_rbp`
        // walked through another address space would fault and lose the report.
        let user_rip = percpu::syscall_rip();
        if percpu::in_syscall() && pid.is_some() {
            log!("  Syscall: num={} user_rip={:#x} user_rsp={:#x}",
                percpu::syscall_num(), user_rip, percpu::user_rsp());
            log!("  User backtrace:");
            process::resolve_user_symbol(user_rip).log_bare(user_rip);
            let pml4 = crate::DirectMap::from_phys(crate::mm::paging::Cr3::current().phys()).as_ptr::<u64>();
            user_backtrace(percpu::syscall_rbp(), pml4, 20);
        }
    }

    if safe_read_u64(ctx.frame.rsp, pml4).is_some() {
        log!("  Stack (from RSP):");
        for i in 0..8u64 {
            let addr = ctx.frame.rsp + i * 8;
            let Some(val) = safe_read_u64(addr, pml4) else { break };
            log!("    [{:#x}] = {:#018x}", addr, val);
        }
    }

    if theirs {
        let crash_addr = if ctx.vector() == Vector::PageFault { ctx.cr2 } else { 0 };
        process::dump_crash_diagnostics(crash_addr, ctx.frame.rip);
    }
}

fn crash_report_panic(info: &core::panic::PanicInfo, rbp: u64) {
    // Must run first: if this panics, only DOUBLE PANIC speaks for it —
    // everything else comes from the copy `panic::record_panic` took.
    if crate::actuator::panic_in_report() {
        panic!("panic-in-report: the crash report panicked before it said anything");
    }
    #[cfg(feature = "boot-actuators")]
    if crate::actuator::fault_in_report() {
        // Canonical, high-half, past any physical memory this kernel boots on:
        // the read faults instead of hitting the direct map.
        const UNMAPPED: u64 = 0xFFFF_8FFF_FFFF_F000;
        // SAFETY: none — deliberately unsafe, staged only when the boot
        // actuator asked for it, to fault a CPU already `Panic` mid-report.
        unsafe { core::ptr::read_volatile(UNMAPPED as *const u64) };
    }
    alert!("PANIC: {}", info);

    log!("  Backtrace:");
    kernel_backtrace(rbp, 20);

    // The address of a local stands in for the stack pointer: this frame is on
    // the crashing stack, which is all the containment test needs.
    let here = 0u64;
    crate::hw::report_contexts(core::ptr::addr_of!(here) as u64, None);

    if let Some(pid) = percpu::current_pid() {
        let tid = percpu::current_tid();
        log!("  Running: pid={} tid={:?}", pid, tid);
        if let Some(guard) = process::PROCESS_TABLE.try_lock() {
            if let Some(table) = guard.as_ref() {
                if let Some(proc) = table.get(pid) {
                    log!("  Process: {} pid={} state={}", proc.name_str(), proc.pid(), if proc.tearing_down() { "TearingDown" } else { "Live" });
                }
            }
        } else {
            log!("  [Process: PROCESS_TABLE locked, skipping]");
        }

        // `in_syscall`, not a non-zero word: these diagnostics belong to the
        // task named above and lie about any other.
        let user_rip = percpu::syscall_rip();
        if percpu::in_syscall() {
            log!("  Syscall: num={} user_rip={:#x} user_rsp={:#x}",
                percpu::syscall_num(), user_rip, percpu::user_rsp());
            log!("  User backtrace:");
            process::resolve_user_symbol(user_rip).log_bare(user_rip);
            let pml4 = crate::DirectMap::from_phys(crate::mm::paging::Cr3::current().phys()).as_ptr::<u64>();
            user_backtrace(percpu::syscall_rbp(), pml4, 20);
        }
    }
}

/// Terminate after a fatal fault, by [`Blame`] — its three states are
/// exhaustive; there is no fourth case to write.
pub(crate) fn recover_or_halt(blame: Blame) -> ! {
    match blame {
        // True user-mode fault — no kernel locks held, safe to use normal exit.
        Blame::Process => {
            percpu::set_fault_state(CpuFaultState::Normal);
            crate::panic::forget();
            syscall::kill_process(-1);
        }
        // Kernel fault on the thread's behalf — may hold locks, use try_lock path.
        Blame::ProcessThroughKernel => try_recover_from_panic(),
        Blame::Kernel => apic::halt_all_cpus(),
    }
}

/// Recovers from a panic in syscall context: poisons the faulted thread for
/// the idle loop to reap, then rejoins the scheduler lock-free.
// Never touches the process table: the faulted thread may hold its lock, so
// only the poison set (read by the idle loop) is safe to use here.
pub(crate) fn try_recover_from_panic() -> ! {
    if let Some(tid) = percpu::current_tid() {
        let pid = percpu::current_pid().unwrap_or(crate::process::Pid(u32::MAX));
        scheduler::poison_tid(scheduler::TaskId(pid, tid));
    }
    percpu::set_fault_state(CpuFaultState::Normal);
    // Clears this CPU's captured fault, the same evidence
    // `panic_console::discard_capture` clears on the panic path: left
    // standing, the next DOUBLE PANIC here would misname an already-survived
    // crash.
    crate::panic::forget();
    scheduler::schedule_no_return();
}


/// Double fault handler — runs on IST1. Always from kernel. Never returns.
pub(super) fn double_fault_handler(frame: &TrapFrame) -> ! {
    let cr2 = cpu::read_cr2();
    let cpu_id = percpu::cpu_id();
    let tid = percpu::current_tid();
    let pid = percpu::current_pid();

    log!("DOUBLE FAULT on CPU {} (pid={:?} tid={:?})", cpu_id, pid, tid);
    log!("  cr2={:#018x} (address that caused the fault chain)", cr2);
    log!("  rip={:#018x}  rsp={:#018x}  rbp={:#018x}", frame.rip, frame.rsp, frame.rbp);
    crate::mm::paging::debug_page_walk(cr2);

    log!("  Kernel backtrace:");
    symbols::resolve_kernel(frame.rip);
    kernel_backtrace_safe(frame.rbp, 20);

    // Stack layout the scan below assumes: entry stubs push [error_code]
    // [vector], then common_entry pushes GPRs — [GPRs 15×8][vector 8]
    // [error_code 8][RIP][CS][RFLAGS][RSP][SS].
    let kernel_rsp = frame.rsp;
    log!("  Scanning kernel stack at {:#x} for original exception context...", kernel_rsp);

    let scan_start = kernel_rsp;
    let scan_end = kernel_rsp.saturating_add(4096);
    let mut addr = scan_start;

    while addr < scan_end {
        let Some(maybe_rip) = safe_read_kernel(addr) else { break };
        let Some(maybe_cs) = safe_read_kernel(addr + 8) else { break };
        let Some(maybe_rflags) = safe_read_kernel(addr + 16) else { break };
        let Some(maybe_rsp) = safe_read_kernel(addr + 24) else { break };

        let valid_cs =
            maybe_cs == u64::from(percpu::KERNEL_CS) || maybe_cs == u64::from(percpu::USER_CS);
        let valid_rflags = maybe_rflags & 2 != 0 && maybe_rflags & !0x3F_FFFF == 0;
        let valid_rip = maybe_rip > 0x1000;

        if valid_cs && valid_rflags && valid_rip {
            let is_user = maybe_cs == u64::from(percpu::USER_CS);
            log!("  Found interrupt frame at stack offset +{:#x}:", addr - kernel_rsp);
            log!("    rip={:#018x}  cs={:#x}  rflags={:#x}", maybe_rip, maybe_cs, maybe_rflags);
            log!("    rsp={:#018x}", maybe_rsp);

            // error_code at addr-8, vector at addr-16, GPRs start at addr-16-15*8.
            let error_code_addr = addr.wrapping_sub(8);
            let saved_regs_base = addr.wrapping_sub(16 + 15 * 8);
            if let Some(error_code) = safe_read_kernel(error_code_addr) {
                log!("    error_code={:#x}", error_code);
            }

            if is_user {
                // Try to recover user RBP from saved GPRs (rbp is at offset 6*8)
                let user_rbp_addr = saved_regs_base + 6 * 8;
                if let Some(user_rbp) = safe_read_kernel(user_rbp_addr) {
                    log!("  User context (pid={:?} tid={:?}):", pid, tid);
                    log!("    rip={:#018x}  rsp={:#018x}  rbp={:#018x}", maybe_rip, maybe_rsp, user_rbp);

                    let pml4 = crate::DirectMap::from_phys(crate::mm::paging::Cr3::current().phys()).as_ptr::<u64>();
                    log!("  User backtrace:");
                    if pid.is_some() {
                        process::resolve_user_symbol(maybe_rip).log_bare(maybe_rip);
                        user_backtrace(user_rbp, pml4, 20);
                    } else {
                        log!("    {:#x}", maybe_rip);
                    }
                }
            } else {
                log!("  Original fault was in kernel code");
                log!("  Kernel backtrace from original fault:");
                symbols::resolve_kernel(maybe_rip);
                let rbp_addr = saved_regs_base + 6 * 8;
                if let Some(orig_rbp) = safe_read_kernel(rbp_addr) {
                    kernel_backtrace_safe(orig_rbp, 20);
                }
            }
            break;
        }

        addr += 8;
    }

    apic::halt_all_cpus();
}

/// #MC halts whichever ring faulted rather than killing a process: there is
/// no instruction to return to, and the reporting state is not trustworthy.
// Untested: firmware leaves CR4.MCE set, so a machine check reaches here, but
// nothing in the suite can stage one.
pub(super) fn machine_check_handler(frame: &TrapFrame) -> ! {
    log!("MACHINE CHECK on CPU {}", percpu::cpu_id());
    let ctx = ExceptionContext { frame, cr2: 0 };
    crash_report(&CrashInfo::Exception(&ctx));
    apic::halt_all_cpus();
}

/// Returns if the fault was resolved (page mapped in); diverges if fatal.
pub(super) fn page_fault_handler(frame: &TrapFrame) {
    let prev = percpu::swap_fault_state(percpu::CpuFaultState::PageFault);
    if prev != percpu::CpuFaultState::Normal {
        // Restores the prior fault state: `fatal_exception` classifies a
        // recursive fault by what it finds, and overwriting Panic/Fatal here
        // would hide the recursion.
        percpu::set_fault_state(prev);
        let cr2 = cpu::read_cr2();
        let ctx = ExceptionContext { frame, cr2 };
        fatal_exception(&ctx);
    }

    let fault_addr = cpu::read_cr2();

    if frame.error_code & PF_PRESENT != 0 && !Ring::of_cs(frame.cs).is_user()
        && mm::is_kernel_addr(fault_addr)
    {
        log!("SMAP cr2={:#018x} rip={:#018x} err={:#018x} rflags={:#018x}",
            fault_addr, frame.rip, frame.error_code, frame.rflags);
        log!("  SMAP kernel backtrace:");
        symbols::resolve_kernel(frame.rip);
        kernel_backtrace(frame.rbp, 20);
    }

    // Only handle not-present faults — protection violations are always fatal
    if frame.error_code & PF_PRESENT == 0 {
        let is_user = Ring::of_cs(frame.cs).is_user();
        if is_user || percpu::current_tid().is_some() {
            if process::handle_page_fault(fault_addr, frame.error_code) {
                percpu::set_fault_state(percpu::CpuFaultState::Normal);
                return;
            }
            log!("#PF UNHANDLED: cr2={:#x} rip={:#x} err={:#x} user={} tid={:?}",
                fault_addr, frame.rip, frame.error_code, is_user, percpu::current_tid());
        } else {
            log!("#PF SKIP: cr2={:#x} rip={:#x} err={:#x} (no tid, not user)",
                fault_addr, frame.rip, frame.error_code);
        }
    } else {
        log!("#PF PRESENT: cr2={:#x} rip={:#x} err={:#x} cs={:#x}",
            fault_addr, frame.rip, frame.error_code, frame.cs);
    }

    let ctx = ExceptionContext { frame, cr2: fault_addr };
    fatal_exception(&ctx);
}


/// Fatal exception handler for #UD and #GP. Never returns.
pub(super) fn exception_handler(frame: &TrapFrame) -> ! {
    let cr2 = if frame.vector == 0x0E { cpu::read_cr2() } else { 0 };
    let ctx = ExceptionContext { frame, cr2 };
    fatal_exception(&ctx);
}

/// Core fatal exception logic. Prints diagnostics, then kills process or halts all CPUs.
fn fatal_exception(ctx: &ExceptionContext) -> ! {
    let blame = ctx.blame();
    let prev = percpu::swap_fault_state(CpuFaultState::Fatal);
    let recursive = prev == CpuFaultState::Fatal || prev == CpuFaultState::Panic;

    // Must run first: a panic anywhere below reaches the panic handler as
    // DOUBLE PANIC, which can only report what was captured here.
    crate::panic::record_fault(
        vector_name(ctx.vector()),
        ctx.frame.rip,
        ctx.cr2,
        ctx.frame.error_code,
    );
    if crate::actuator::panic_in_report() {
        panic!("panic-in-report: the crash report panicked before it said anything");
    }

    let tid_raw = percpu::current_tid().map_or(u32::MAX, |t| t.raw());
    if recursive {
        alert!("FAULT rip={:#018x} cr2={:#018x} err={:#018x} cr3={:#018x} rsp={:#018x} tid={} RECURSIVE",
            ctx.frame.rip, ctx.cr2, ctx.frame.error_code, cpu::read_cr3(), ctx.frame.rsp, tid_raw);
    } else {
        alert!("FAULT rip={:#018x} cr2={:#018x} err={:#018x} cr3={:#018x} rsp={:#018x} tid={}",
            ctx.frame.rip, ctx.cr2, ctx.frame.error_code, cpu::read_cr3(), ctx.frame.rsp, tid_raw);
    }

    // Recursive fault: no second report. Even ProcessThroughKernel can't
    // survive `try_recover_from_panic`'s rejoin here, so end the process or halt.
    if recursive {
        if blame != Blame::Kernel {
            percpu::set_fault_state(CpuFaultState::Normal);
            crate::panic::forget();
            syscall::kill_process(-1);
        }
        apic::halt_all_cpus();
    }

    crash_report(&CrashInfo::Exception(ctx));
    recover_or_halt(blame);
}
