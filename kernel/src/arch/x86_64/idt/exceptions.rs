use crate::arch::{cpu, percpu};
use crate::syscall;
use crate::arch::percpu::CpuFaultState;
use crate::{alert, log, mm, process, symbols};
use crate::symbols::kernel_backtrace;

use toyos_userbound::Ring;

use super::{Vector, TrapFrame, PF_PRESENT, PF_WRITE, PF_INSTRUCTION_FETCH};

/// Walk RBP chain for user backtrace through page tables. Takes no pid: this
/// always backtraces the process running on this CPU.
fn user_backtrace(start_rbp: u64, max_frames: usize) {
    let mut rbp = start_rbp;
    for _ in 0..max_frames {
        if rbp == 0 { break; }
        let saved_rbp = match read_user_u64(rbp) {
            Ok(word) => word,
            Err(Unread::Refused) => {
                log!("    rbp {:#x} refused: no user address", rbp);
                break;
            }
            Err(Unread::Absent) => break,
        };
        let Ok(return_addr) = read_user_u64(rbp + 8) else { break };
        if return_addr == 0 { break; }
        process::record_user_frame_return(return_addr).log_bare(return_addr);
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

/// Why the crash report read no word at an address.
#[derive(Clone, Copy)]
enum Unread {
    /// Not a user address: a crash report is never how a process reads the kernel.
    Refused,
    /// Misaligned, or nothing mapped there.
    Absent,
}

/// A word of the faulting process's memory, read through the tables this CPU
/// runs under: the crash path may neither take the address space's lock nor
/// demand-page.
fn read_user_u64(addr: u64) -> Result<u64, Unread> {
    if !toyos_userbound::is_user_addr(addr) {
        return Err(Unread::Refused);
    }
    if !addr.is_multiple_of(8) {
        return Err(Unread::Absent);
    }
    let at = mm::paging::translate_in_current_tables(addr).ok_or(Unread::Absent)?;
    // SAFETY: a direct-map address of 8 aligned bytes inside the present leaf
    // the current tables resolved. Not `read_volatile`: see `kernel_backtrace`.
    Ok(unsafe { *at.as_ptr::<u64>() })
}

/// The `in` or `out` at `rip`, read through the page tables as the rest of the
/// report reads user memory; `None` for any other instruction or an unreadable one.
fn port_access_at(rip: u64, rdx: u64) -> Option<toyos_userbound::PortAccess> {
    let (at, shift) = (rip & !7, rip & 7);
    let lo = read_user_u64(at).ok()?;
    // The next word only where the four bytes run into it: an `in` that ends
    // just before an unmapped page is still named.
    let hi = if shift > 4 { read_user_u64(at + 8).ok()? } else { 0 };
    // Shifted rather than indexed: nothing on this path may panic.
    let code = ((u128::from(lo) | u128::from(hi) << 64) >> (8 * shift)) as u32;
    toyos_userbound::port_access(code.to_le_bytes(), rdx as u16)
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
    let ring3 = ctx.ring().is_user();
    let tid = percpu::current_tid().unwrap_or(crate::process::Tid(0));
    let pid = percpu::current_pid();

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

    if ring3 {
        match ctx.vector() {
            Vector::PageFault => log!("SEGFAULT tid={}: {} {} at {:#x}", tid, pf_action, pf_cause, ctx.cr2),
            Vector::InvalidOpcode => log!("SIGILL tid={}: illegal instruction", tid),
            Vector::DivideError | Vector::X87FloatingPoint | Vector::SimdFloatingPoint => {
                log!("SIGFPE tid={}: {}", tid, name)
            }
            Vector::GeneralProtection | Vector::StackSegment | Vector::AlignmentCheck => {
                log!("SIGBUS tid={}: {} (error_code={:#x})", tid, name, ctx.frame.error_code);
                // At CPL 3 an `in` or `out` faults only on a port the bitmap refuses.
                if let Some(access) = (ctx.vector() == Vector::GeneralProtection)
                    .then(|| port_access_at(ctx.frame.rip, ctx.frame.rdx))
                    .flatten()
                {
                    match super::super::pio::refused_port(access) {
                        Some(port) if port == access.port => {
                            log!("  {access}, which this process holds no grant for")
                        }
                        Some(port) => log!(
                            "  {access}, reaching port {port:#06x}, which this process holds no grant for"
                        ),
                        // The bitmap opens the span whole, so the #GP is not
                        // the port's: a string form's non-canonical `rsi`/`rdi`, say.
                        None => log!("  {access}, a #GP this decode cannot attribute to a port"),
                    }
                }
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
            process::record_user_frame(ctx.frame.rip).log_bare(ctx.frame.rip);
        } else {
            log!("    {:#x}", ctx.frame.rip);
        }
    } else {
        symbols::resolve_kernel(ctx.frame.rip);
    }

    if ctx.vector() == Vector::PageFault {
        // A user fault walks its own half: the kernel's tables are no process's to read.
        if ring3 && !toyos_userbound::is_user_addr(ctx.cr2) {
            log!("  Page walk for {:#x} refused: no user address", ctx.cr2);
        } else {
            crate::mm::paging::debug_page_walk(ctx.cr2);
        }
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
    if !ring3 {
        crate::hw::report_contexts(ctx.frame.rsp, None);
    }

    log!("  Backtrace:");
    if ring3 {
        if pid.is_some() {
            user_backtrace(ctx.frame.rbp, 32);
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
            process::record_user_frame(user_rip).log_bare(user_rip);
            user_backtrace(percpu::syscall_rbp(), 20);
        }
    }

    // A user fault's stack is the process's own, and a stack pointer it aimed
    // at the kernel reads nothing.
    let read = |addr: u64| if ring3 { read_user_u64(addr) } else { safe_read_kernel(addr).ok_or(Unread::Absent) };
    match read(ctx.frame.rsp) {
        Err(Unread::Refused) => log!("  Stack (from RSP): {:#x} refused: no user address", ctx.frame.rsp),
        Err(Unread::Absent) => {}
        Ok(_) => {
            log!("  Stack (from RSP):");
            for i in 0..8u64 {
                let addr = ctx.frame.rsp.wrapping_add(i * 8);
                let Ok(val) = read(addr) else { break };
                log!("    [{:#x}] = {:#018x}", addr, val);
            }
        }
    }

    if ring3 {
        let crash_addr = if ctx.vector() == Vector::PageFault { ctx.cr2 } else { 0 };
        process::dump_crash_diagnostics(crash_addr, ctx.frame.rip);
    }
}

fn crash_report_panic(info: &core::panic::PanicInfo, rbp: u64) {
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
            process::record_user_frame(user_rip).log_bare(user_rip);
            user_backtrace(percpu::syscall_rbp(), 20);
        }
    }
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

                    log!("  User backtrace:");
                    if pid.is_some() {
                        process::record_user_frame(maybe_rip).log_bare(maybe_rip);
                        user_backtrace(user_rbp, 20);
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

    crate::panic::halt_all_cpus();
}

/// #MC halts whichever ring faulted rather than killing a process: there is
/// no instruction to return to, and the reporting state is not trustworthy.
// Untested: firmware leaves CR4.MCE set, so a machine check reaches here, but
// nothing in the suite can stage one.
pub(super) fn machine_check_handler(frame: &TrapFrame) -> ! {
    log!("MACHINE CHECK on CPU {}", percpu::cpu_id());
    let ctx = ExceptionContext { frame, cr2: 0 };
    crash_report(&CrashInfo::Exception(&ctx));
    crate::panic::halt_all_cpus();
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
        // Ring 3 only: a Ring 0 fault is a kernel bug, and it halts below
        // before anything is mapped into whichever process is current.
        if is_user && process::handle_page_fault(fault_addr, frame.error_code) {
            percpu::set_fault_state(percpu::CpuFaultState::Normal);
            return;
        }
        log!("#PF UNHANDLED: cr2={:#x} rip={:#x} err={:#x} user={} tid={:?}",
            fault_addr, frame.rip, frame.error_code, is_user, percpu::current_tid());
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

    let tid_raw = percpu::current_tid().map_or(u32::MAX, |t| t.raw());
    if recursive {
        alert!("FAULT rip={:#018x} cr2={:#018x} err={:#018x} cr3={:#018x} rsp={:#018x} tid={} RECURSIVE",
            ctx.frame.rip, ctx.cr2, ctx.frame.error_code, cpu::read_cr3(), ctx.frame.rsp, tid_raw);
    } else {
        alert!("FAULT rip={:#018x} cr2={:#018x} err={:#018x} cr3={:#018x} rsp={:#018x} tid={}",
            ctx.frame.rip, ctx.cr2, ctx.frame.error_code, cpu::read_cr3(), ctx.frame.rsp, tid_raw);
    }

    // Recursive fault: no second report.
    if recursive {
        crate::panic::halt_all_cpus();
    }

    crash_report(&CrashInfo::Exception(ctx));
    // A Ring 3 fault holds no kernel lock, so the ordinary exit ends its
    // process; every Ring 0 fault is the kernel's, whatever thread is current.
    if ctx.ring().is_user() {
        percpu::set_fault_state(CpuFaultState::Normal);
        crate::panic::forget();
        syscall::kill_process(-1);
    }
    crate::panic::halt_all_cpus();
}
