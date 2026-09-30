//! Exceptions: the vector table `VBAR_EL1` names, and what each one taken
//! comes to.
//!
//! Every entry saves the interrupted context into a [`Frame`] on the stack
//! it was taken on — for an entry from EL0, `SP_EL1` as the last `ERET` left
//! it, the top of the running thread's kernel stack — and [`dispatch`] routes
//! it: an interrupt to [`irq`], an `SVC` to the syscall dispatcher, a
//! translation fault to the demand pager, anything else from EL0 to the end
//! of its process, and anything else from EL1 to a panic. Every return to EL0
//! runs `scheduler::exit_to_user` last. The entry saves no FP/SIMD register:
//! the kernel never touches one, and a switch saves the thread's
//! (`super::switch`).
//!
//! A table that does not reach [`dispatch`] — misaligned, or never
//! installed — is what the aarch64 boot test's fault arm catches, because
//! then nothing reports at all.

use core::fmt;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};

use toyos_sched::hw::{CpuId, Machine, TraceEvent, TraceKind};

use super::percpu::{self, CpuFaultState};
use super::{cpu, irqchip};
use crate::irq_census::Source;
use crate::{alert, log};

/// The interrupted context, as the entry stores it: `x0`–`x30`, the stack
/// pointer before the exception, then the four system registers that say
/// what happened.
#[repr(C)]
pub struct Frame {
    pub x: [u64; 31],
    pub sp: u64,
    pub elr: u64,
    pub spsr: u64,
    pub esr: u64,
    pub far: u64,
}

const FRAME_BYTES: usize = core::mem::size_of::<Frame>();
const _: () = assert!(FRAME_BYTES == 288 && FRAME_BYTES.is_multiple_of(16));

/// The table's entries this kernel acts on: Arm ARM K.a, D1.3.1, Table D1-7.
const EL1_SYNC: u64 = 4;
const EL1_IRQ: u64 = 5;
const EL0_SYNC: u64 = 8;
const EL0_IRQ: u64 = 9;

/// `ESR_EL1.EC` of the classes an EL0 entry acts on.
const EC_SVC64: u64 = 0x15;
const EC_IABT_LOWER: u64 = 0x20;
const EC_DABT_LOWER: u64 = 0x24;

/// Which of the table's sixteen entries was taken: four groups by where the
/// exception came from, and in each the synchronous, IRQ, FIQ and SError entries.
#[derive(Clone, Copy)]
struct Entry(u64);

impl fmt::Display for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let from = ["EL1 on SP_EL0", "EL1 on SP_EL1", "EL0 in AArch64", "EL0 in AArch32"];
        let kind = ["synchronous", "IRQ", "FIQ", "SError"];
        write!(f, "{} from {}", kind[(self.0 & 3) as usize], from[(self.0 >> 2) as usize & 3])
    }
}

/// `ESR_ELx.EC`, the exception class, named for the report: Arm ARM K.a,
/// D24.2.40, the classes this kernel can take at EL1.
fn class_name(esr: u64) -> &'static str {
    match esr >> 26 {
        0x00 => "unknown reason (an undefined instruction)",
        0x01 => "WFI or WFE trapped",
        0x07 => "SIMD or floating point trapped",
        0x0E => "illegal execution state",
        0x15 => "SVC from AArch64",
        0x18 => "system register access trapped",
        0x19 => "SVE trapped",
        0x20 => "instruction abort from a lower EL",
        0x21 => "instruction abort",
        0x22 => "PC alignment fault",
        0x24 => "data abort from a lower EL",
        0x25 => "data abort",
        0x26 => "SP alignment fault",
        0x2F => "SError",
        0x3C => "BRK",
        _ => "an exception class this kernel does not name",
    }
}

/// The Rust half of every vector entry. Returns to the entry, which restores
/// the frame and returns from the exception; everything fatal diverges.
extern "C" fn dispatch(frame: &mut Frame, entry: u64) {
    // Each returning entry found interrupts open, an IRQ because only then is
    // one taken and EL0 because it never masks them, and taking it masked
    // them; `exit_to_user` opens them for EL0, and the `ERET` for EL1.
    match entry {
        EL1_IRQ => {
            #[cfg(feature = "mask-windows")]
            crate::windows::irqs_masked();
            irq(false);
            #[cfg(feature = "mask-windows")]
            crate::windows::irqs_unmasking();
        }
        EL0_SYNC => {
            #[cfg(feature = "mask-windows")]
            crate::windows::irqs_masked();
            el0_sync(frame);
            crate::scheduler::exit_to_user();
        }
        EL0_IRQ => {
            #[cfg(feature = "mask-windows")]
            crate::windows::irqs_masked();
            irq(true);
            crate::scheduler::exit_to_user();
        }
        _ => exception(frame, entry),
    }
}

/// The report of an exception this kernel does not return from: its own, at
/// EL1, or an FIQ or SError from anywhere. The panic handler's branch puts it
/// on the console and the panel.
fn exception(frame: &Frame, entry: u64) -> ! {
    let entry = Entry(entry);
    log!("KERNEL PANIC: {entry}: {} (ESR={:#010x})", class_name(frame.esr), frame.esr);
    log!("  elr={:#018x}  far={:#018x}  spsr={:#010x}", frame.elr, frame.far, frame.spsr);
    for pair in 0..15 {
        log!(
            "  x{:<2}={:#018x}  x{:<2}={:#018x}",
            pair * 2,
            frame.x[pair * 2],
            pair * 2 + 1,
            frame.x[pair * 2 + 1]
        );
    }
    log!("  x30={:#018x}  sp ={:#018x}", frame.x[30], frame.sp);
    if entry.0 == EL1_SYNC && matches!(frame.esr >> 26, 0x21 | 0x25) && crate::log::PERCPU_READY.load(Relaxed) {
        super::paging::debug_page_walk(frame.far);
    }
    crate::symbols::kernel_backtrace(frame.x[29], 20);
    panic!("{entry}: {} at {:#x}", class_name(frame.esr), frame.elr);
}

/// One interrupt. From EL0 a tick or a kick
/// preempts here, where the interrupted context holds nothing; from EL1 it
/// only asks for the pass the context will run when it may.
fn irq(from_el0: bool) {
    let Some(intid) = irqchip::acknowledge() else {
        percpu::irq_took(Source::Spurious);
        return;
    };
    if intid == irqchip::timer_intid() {
        // Before anything that can take a lock or panic: a timer left
        // asserted re-fires forever, and one left stopped never fires again.
        irqchip::rearm();
        percpu::irq_took(Source::Timer);
        // Before anything that can take a lock, in both levels: a CPU spinning
        // on one still takes this interrupt, which is why the poll is here.
        crate::deadline::poll();
        #[cfg(feature = "boot-actuators")]
        storm::tick();
        if from_el0 {
            // Only an EL0 tick reaches here, so the interrupted context is user
            // code and holds no `Lock`.
            assert_eq!(crate::preempt::count(), 0, "a timer interrupt from EL0 found a preempt depth");
            let hw = &crate::hw::HW;
            hw.trace(TraceEvent { ts: hw.now(), cpu: CpuId(percpu::cpu_id()), kind: TraceKind::TimerFire });
            irqchip::end(intid);
            crate::scheduler::do_preempt();
        } else {
            crate::preempt::set_need_resched();
            percpu::note_kernel_timer_fire();
            irqchip::end(intid);
        }
        return;
    }
    match intid {
        // Never ended: the running priority it keeps is every interrupt's
        // own, so the interface signals this CPU nothing and the halt stays.
        irqchip::SGI_HALT => cpu::halt(),
        irqchip::SGI_KICK => {
            percpu::irq_took(Source::Timer);
            irqchip::end(intid);
            if from_el0 {
                crate::scheduler::do_preempt();
            } else {
                crate::preempt::set_need_resched();
            }
        }
        #[cfg(feature = "boot-actuators")]
        intid if intid == u32::from(LOG_NEST_VECTOR) => {
            percpu::preempt_count_up();
            crate::log::nested::deliver();
            percpu::preempt_count_down();
            irqchip::end(intid);
        }
        #[cfg(feature = "boot-actuators")]
        irqchip::SGI_STORM => {
            storm::sgi();
            irqchip::end(intid);
        }
        _ => {
            percpu::irq_took(Source::Unclaimed);
            UNCLAIMED.fetch_add(1, Relaxed);
            LAST_UNCLAIMED.store(intid, Relaxed);
            irqchip::end(intid);
        }
    }
}

/// Interrupts no handler here claims, and the last one's INTID.
static UNCLAIMED: AtomicU64 = AtomicU64::new(0);
static LAST_UNCLAIMED: AtomicU32 = AtomicU32::new(0);
/// The count at the last report; process exit logs once per batch.
static UNCLAIMED_REPORTED: AtomicU64 = AtomicU64::new(0);

pub(crate) fn log_unclaimed() {
    let count = UNCLAIMED.load(Relaxed);
    if count == 0 || UNCLAIMED_REPORTED.swap(count, Relaxed) == count {
        return;
    }
    log!("irq: unclaimed interrupts={count}, the last INTID {}", LAST_UNCLAIMED.load(Relaxed));
}

/// A synchronous exception from EL0: a syscall, a fault the demand pager may
/// serve, or the end of the process.
fn el0_sync(frame: &mut Frame) {
    match frame.esr >> 26 {
        EC_SVC64 => syscall(frame),
        EC_IABT_LOWER | EC_DABT_LOWER => user_abort(frame),
        _ => {
            percpu::preempt_count_up();
            user_fatal(frame);
        }
    }
}

/// `SVC #0`: the number in `x0`, four arguments in `x1`–`x4`, the answer
/// back in `x0`, and every other register the thread's as it left it.
fn syscall(frame: &mut Frame) {
    percpu::enter_syscall(frame.elr, frame.x[0], frame.x[29], frame.sp);
    percpu::preempt_count_up();
    let answer = crate::syscall::dispatch::syscall_dispatch(frame.x[0], frame.x[1], frame.x[2], frame.x[3], frame.x[4]);
    percpu::preempt_count_down();
    percpu::leave_syscall();
    frame.x[0] = answer;
}

/// `DFSC`/`IFSC` levels 0 to 3 of a translation fault: nothing mapped there.
fn is_translation_fault(esr: u64) -> bool {
    esr & 0b11_1100 == 0b00_0100
}

/// An abort from EL0: a translation fault in a region the demand pager
/// fills, or the end of the process.
fn user_abort(frame: &mut Frame) {
    percpu::preempt_count_up();
    // `FnV`: a data abort whose `FAR_EL1` is not valid names no address to fill.
    let far_valid = frame.esr >> 26 == EC_IABT_LOWER || frame.esr & (1 << 10) == 0;
    if is_translation_fault(frame.esr) && far_valid {
        let prev = percpu::swap_fault_state(CpuFaultState::PageFault);
        if prev != CpuFaultState::Normal {
            // Put back: `user_fatal` classifies a recursive fault by what it finds.
            percpu::set_fault_state(prev);
            user_fatal(frame);
        }
        cpu::enable_interrupts();
        let served = crate::process::handle_page_fault(frame.far, frame.esr);
        cpu::disable_interrupts();
        if served {
            percpu::set_fault_state(CpuFaultState::Normal);
            percpu::preempt_count_down();
            return;
        }
        log!(
            "#PF UNHANDLED: far={:#x} pc={:#x} esr={:#x} tid={:?}",
            frame.far,
            frame.elr,
            frame.esr,
            percpu::current_tid()
        );
    }
    user_fatal(frame);
}

/// A fault from EL0 nothing serves: report it, and end the process it came
/// from; a second fault while this one reports halts the machine.
fn user_fatal(frame: &Frame) -> ! {
    let prev = percpu::swap_fault_state(CpuFaultState::Fatal);
    let recursive = matches!(prev, CpuFaultState::Fatal | CpuFaultState::Panic);
    // First: a panic anywhere below reaches the panic handler as DOUBLE
    // PANIC, which can only report what was captured here.
    crate::panic::record_fault(class_name(frame.esr), frame.elr, frame.far, frame.esr);
    if crate::actuator::panic_in_report() {
        panic!("panic-in-report: the crash report panicked before it said anything");
    }
    let tid = percpu::current_tid().map_or(u32::MAX, |t| t.raw());
    alert!(
        "FAULT pc={:#018x} far={:#018x} esr={:#010x} sp={:#018x} tid={tid}{}",
        frame.elr,
        frame.far,
        frame.esr,
        frame.sp,
        if recursive { " RECURSIVE" } else { "" }
    );
    if recursive {
        crate::panic::halt_all_cpus();
    }
    user_report(frame);
    percpu::set_fault_state(CpuFaultState::Normal);
    crate::panic::forget();
    crate::syscall::kill_process(-1);
}

/// What a fault from EL0 says about the thread: the signal it would be
/// elsewhere, its registers, and its frames, read without a lock.
fn user_report(frame: &Frame) {
    let tid = percpu::current_tid().map_or(u32::MAX, |t| t.raw());
    let class = frame.esr >> 26;
    match class {
        EC_IABT_LOWER | EC_DABT_LOWER => {
            let action = if class == EC_IABT_LOWER {
                "execute"
            } else if frame.esr & (1 << 6) != 0 {
                "write"
            } else {
                "read"
            };
            let cause = match frame.esr & 0b11_1100 {
                0b00_0100 => "unmapped address",
                0b00_1100 => "protection violation",
                _ => "fault",
            };
            log!("SEGFAULT tid={tid}: {action} {cause} at {:#x} (FSC {:#x})", frame.far, frame.esr & 0x3F);
        }
        0x00 => log!("SIGILL tid={tid}: illegal instruction"),
        0x22 | 0x26 => log!("SIGBUS tid={tid}: {}", class_name(frame.esr)),
        0x3C => log!("SIGTRAP tid={tid}: BRK #{:#x}", frame.esr & 0xFFFF),
        _ => log!("FATAL tid={tid}: {} (ESR={:#010x})", class_name(frame.esr), frame.esr),
    }
    log!("  pc:");
    if percpu::current_pid().is_some() {
        crate::process::resolve_user_symbol(frame.elr).log_bare(frame.elr);
    } else {
        log!("    {:#x}", frame.elr);
    }
    if matches!(class, EC_IABT_LOWER | EC_DABT_LOWER) {
        super::paging::debug_page_walk(frame.far);
    }
    log!("  Registers:");
    for pair in 0..15 {
        log!("    x{:<2}={:#018x}  x{:<2}={:#018x}", pair * 2, frame.x[pair * 2], pair * 2 + 1, frame.x[pair * 2 + 1]);
    }
    log!("    x30={:#018x}  sp={:#018x}  spsr={:#010x}", frame.x[30], frame.sp, frame.spsr);
    log!("  Backtrace:");
    user_backtrace(frame.x[29], 32);
    let crash_addr = if matches!(class, EC_IABT_LOWER | EC_DABT_LOWER) { frame.far } else { 0 };
    crate::process::dump_crash_diagnostics(crash_addr, frame.elr);
}

/// The user frame-pointer chain from `fp`: each frame's saved `x29` at `fp`
/// and its return address at `fp + 8`, read through the running space's tables.
fn user_backtrace(start_fp: u64, max_frames: usize) {
    let mut fp = start_fp;
    for _ in 0..max_frames {
        let Some(saved) = super::paging::read_user_word(fp) else { break };
        let Some(ret) = super::paging::read_user_word(fp + 8) else { break };
        if ret == 0 {
            break;
        }
        crate::process::resolve_user_symbol_return(ret).log_bare(ret);
        fp = saved;
    }
}

// The table: sixteen entries of 0x80 bytes, 2 KiB aligned (`VBAR_EL1` bits
// 10:0 are RES0). Each entry makes room for a `Frame`, saves x0/x1, names
// itself in x1 and branches to the common save, which fills in the rest, calls
// `dispatch` with the frame in x0, and restores whatever it returns to. The
// stack an entry from EL0 names is `SP_EL0`, and a return to EL0 (`SPSR_EL1.M`
// zero) puts the frame's back.
core::arch::global_asm!(
    ".macro toyos_vector n",
    ".balign 0x80",
    "sub sp, sp, #{frame}",
    "stp x0, x1, [sp, #0]",
    "mov x1, #\\n",
    "b 1f",
    ".endm",
    ".pushsection .text.toyos_vectors, \"ax\"",
    ".balign 0x800",
    ".global toyos_vectors",
    "toyos_vectors:",
    "toyos_vector 0", "toyos_vector 1", "toyos_vector 2", "toyos_vector 3",
    "toyos_vector 4", "toyos_vector 5", "toyos_vector 6", "toyos_vector 7",
    "toyos_vector 8", "toyos_vector 9", "toyos_vector 10", "toyos_vector 11",
    "toyos_vector 12", "toyos_vector 13", "toyos_vector 14", "toyos_vector 15",
    "1:",
    "stp x2, x3, [sp, #16]",
    "stp x4, x5, [sp, #32]",
    "stp x6, x7, [sp, #48]",
    "stp x8, x9, [sp, #64]",
    "stp x10, x11, [sp, #80]",
    "stp x12, x13, [sp, #96]",
    "stp x14, x15, [sp, #112]",
    "stp x16, x17, [sp, #128]",
    "stp x18, x19, [sp, #144]",
    "stp x20, x21, [sp, #160]",
    "stp x22, x23, [sp, #176]",
    "stp x24, x25, [sp, #192]",
    "stp x26, x27, [sp, #208]",
    "stp x28, x29, [sp, #224]",
    "tbnz x1, #3, 2f",
    "add x2, sp, #{frame}",
    "b 3f",
    "2:",
    "mrs x2, sp_el0",
    "3:",
    "stp x30, x2, [sp, #240]",
    "mrs x2, elr_el1",
    "mrs x3, spsr_el1",
    "stp x2, x3, [sp, #256]",
    "mrs x2, esr_el1",
    "mrs x3, far_el1",
    "stp x2, x3, [sp, #272]",
    "mov x0, sp",
    "bl {dispatch}",
    "ldp x2, x3, [sp, #256]",
    "msr elr_el1, x2",
    "msr spsr_el1, x3",
    "ldr x2, [sp, #248]",
    "tst x3, #0xF",
    "b.ne 4f",
    "msr sp_el0, x2",
    "4:",
    "ldp x2, x3, [sp, #16]",
    "ldp x4, x5, [sp, #32]",
    "ldp x6, x7, [sp, #48]",
    "ldp x8, x9, [sp, #64]",
    "ldp x10, x11, [sp, #80]",
    "ldp x12, x13, [sp, #96]",
    "ldp x14, x15, [sp, #112]",
    "ldp x16, x17, [sp, #128]",
    "ldp x18, x19, [sp, #144]",
    "ldp x20, x21, [sp, #160]",
    "ldp x22, x23, [sp, #176]",
    "ldp x24, x25, [sp, #192]",
    "ldp x26, x27, [sp, #208]",
    "ldp x28, x29, [sp, #224]",
    "ldr x30, [sp, #240]",
    "ldp x0, x1, [sp, #0]",
    "add sp, sp, #{frame}",
    "eret",
    ".popsection",
    frame = const FRAME_BYTES,
    dispatch = sym dispatch,
);

/// Point `VBAR_EL1` at the table. Called by the entry before anything that can fault.
pub fn install() {
    // SAFETY: the table is 2 KiB aligned, lives in the kernel image for the
    // life of the machine, and every entry ends in `dispatch`. The `ISB` makes
    // the new base the one the next exception uses.
    unsafe {
        core::arch::asm!(
            "adrp {t}, toyos_vectors",
            "add {t}, {t}, :lo12:toyos_vectors",
            "msr vbar_el1, {t}",
            "isb",
            t = out(reg) _,
            options(nostack, preserves_flags),
        );
    }
}

pub const HDA_VECTOR: u8 = irqchip::Intid::Hda as u8;
pub const VIRTIO_SOUND_VECTOR: u8 = irqchip::Intid::VirtioSound as u8;
pub const LOG_NEST_VECTOR: u8 = irqchip::Intid::LogNest as u8;

/// The crash report for a panic, from the frame pointer the panic handler
/// stood on: the backtrace, which CPU is on which stack, and what the
/// running thread was doing.
pub(crate) fn report_panic(message: &core::panic::PanicInfo, frame: u64) {
    alert!("PANIC: {}", message);
    log!("  Backtrace:");
    crate::symbols::kernel_backtrace(frame, 20);
    crate::hw::report_contexts(cpu::stack_pointer(), None);
    let Some(pid) = percpu::current_pid() else { return };
    log!("  Running: pid={} tid={:?}", pid, percpu::current_tid());
    if percpu::in_syscall() {
        let (pc, fp, sp) = percpu::syscall_context();
        log!("  Syscall: num={} user_pc={pc:#x} user_sp={sp:#x}", percpu::syscall_num());
        log!("  User backtrace:");
        crate::process::resolve_user_symbol(pc).log_bare(pc);
        user_backtrace(fp, 20);
    }
}

/// Whether the interrupted context an `SPSR` describes could have taken an
/// interrupt: `SPSR.I` clear.
pub(crate) const fn frame_interrupts_enabled(spsr: u64) -> bool {
    spsr & (1 << 7) == 0
}

/// Nothing to report: the vectors run on the stack they interrupted.
pub(crate) fn report_fault_stack() {}

/// x86-64's lands a `#DF` on its IST stack; AArch64 has no double fault, and
/// an exception on a broken stack takes the same vector again, so the call is
/// refused.
#[cfg(feature = "test-actuators")]
pub(crate) fn provoke_double_fault() -> u64 {
    toyos_abi::syscall::SyscallError::NotSupported.to_u64()
}

/// `irq-storm`: this CPU floods itself with SGIs, sending each as soon as the
/// last is taken, until the timer has fired `TICKS_OWED` times through the
/// flood, then waits for the last SGI to be taken. One at a time, because an
/// SGI sent while the last is still pending merges with it. A tick lost, or
/// never re-armed, leaves the flood running and an SGI lost leaves the wait
/// running, so neither says anything: the harness's ceiling is the only clock
/// this judges by, and the one verdict the storm can say is `FAIL` for an SGI
/// taken that was never sent.
#[cfg(feature = "boot-actuators")]
pub(crate) mod storm {
    use core::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

    use super::irqchip;
    use crate::log;

    static RUNNING: AtomicBool = AtomicBool::new(false);
    static TICKS: AtomicU64 = AtomicU64::new(0);
    static SGIS: AtomicU64 = AtomicU64::new(0);

    pub(super) fn tick() {
        if RUNNING.load(Relaxed) {
            TICKS.fetch_add(1, Relaxed);
        }
    }

    pub(super) fn sgi() {
        SGIS.fetch_add(1, Relaxed);
    }

    /// The timer's period, which each tick re-arms.
    const PERIOD_NS: u64 = 1_000_000;
    /// The ticks the flood lasts for.
    const TICKS_OWED: u64 = 1_000;

    /// Run the storm, with interrupts open on this CPU until it ends, and say
    /// what it counted.
    pub fn run() {
        let _guard = crate::arch::IrqGuard::close();
        TICKS.store(0, Relaxed);
        SGIS.store(0, Relaxed);
        irqchip::arm_one_shot(PERIOD_NS);
        RUNNING.store(true, Relaxed);
        let mut sent = 0u64;
        crate::arch::cpu::enable_interrupts();
        while TICKS.load(Relaxed) < TICKS_OWED {
            if SGIS.load(Relaxed) == sent {
                irqchip::send_self(irqchip::SGI_STORM as u8);
                sent += 1;
            }
        }
        RUNNING.store(false, Relaxed);
        irqchip::stop_timer();
        while SGIS.load(Relaxed) < sent {
            core::hint::spin_loop();
        }
        crate::arch::cpu::disable_interrupts();
        let (ticks, taken) = (TICKS.load(Relaxed), SGIS.load(Relaxed));
        let verdict = if taken == sent { "PASS" } else { "FAIL" };
        log!("irq-storm: {verdict} sgis={taken}/{sent} ticks={ticks}: the timer fired through the flood, and every SGI sent was taken");
    }
}
