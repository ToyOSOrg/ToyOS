//! Exceptions: the vector table `VBAR_EL1` names, and what the kernel says
//! when one is taken.
//!
//! Every exception is fatal until the port's stage 4 gives interrupts and
//! user faults somewhere to go: the entry saves the interrupted context into a
//! [`Frame`] on the stack it was taken on, and [`exception`] reports it and
//! panics. A table that does not reach [`exception`] — misaligned, or never
//! installed — is what the aarch64 boot test's fault arm catches, because
//! then nothing reports at all.

use core::fmt;

use crate::log;

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

/// Which of the table's sixteen entries was taken: Arm ARM K.a, D1.3.1,
/// Table D1-7 — four groups by where the exception came from, and in each the
/// synchronous, IRQ, FIQ and SError entries.
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

/// The Rust half of every vector entry: report the context and panic. The
/// panic handler's early branch puts the report on the console and the panel.
extern "C" fn exception(frame: &Frame, entry: u64) -> ! {
    let entry = Entry(entry);
    log!(
        "KERNEL PANIC: {entry}: {} (ESR={:#010x})",
        class_name(frame.esr),
        frame.esr
    );
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
    crate::symbols::kernel_backtrace(frame.x[29], 20);
    panic!("{entry}: {} at {:#x}", class_name(frame.esr), frame.elr);
}

// The table: sixteen entries of 0x80 bytes, 2 KiB aligned (`VBAR_EL1` bits
// 10:0 are RES0). Each entry makes room for a `Frame`, saves x0/x1, names
// itself in x1 and branches to the common save, which fills in the rest and
// calls `exception` with the frame in x0.
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
    "add x2, sp, #{frame}",
    "stp x30, x2, [sp, #240]",
    "mrs x2, elr_el1",
    "mrs x3, spsr_el1",
    "stp x2, x3, [sp, #256]",
    "mrs x2, esr_el1",
    "mrs x3, far_el1",
    "stp x2, x3, [sp, #272]",
    "mov x0, sp",
    "bl {exception}",
    "brk #0",
    ".popsection",
    frame = const FRAME_BYTES,
    exception = sym exception,
);

/// Point `VBAR_EL1` at the table. Called by the entry before anything that can fault.
pub fn install() {
    // SAFETY: the table is 2 KiB aligned, lives in the kernel image for the
    // life of the machine, and every entry ends in `exception`, which never
    // returns. The `ISB` makes the new base the one the next exception uses.
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

/// Interrupt identifiers the generic drivers program. Inert until stage 4
/// assigns them to GIC interrupts: every path that would deliver one
/// ([`super::msi_message`], [`super::irqchip::send_self`]) is owed.
#[repr(u8)]
enum Vector {
    LogNest = 1,
    Hda,
    VirtioSound,
}

pub const HDA_VECTOR: u8 = Vector::Hda as u8;
pub const VIRTIO_SOUND_VECTOR: u8 = Vector::VirtioSound as u8;
pub const LOG_NEST_VECTOR: u8 = Vector::LogNest as u8;

/// The crash report for a panic, from the frame pointer the panic handler stood on.
pub(crate) fn report_panic(message: &core::panic::PanicInfo, frame: u64) {
    crate::alert!("PANIC: {}", message);
    log!("  Backtrace:");
    crate::symbols::kernel_backtrace(frame, 20);
}

/// Whether the interrupted context an `SPSR` describes could have taken an
/// interrupt: `SPSR.I` clear.
pub(crate) const fn frame_interrupts_enabled(spsr: u64) -> bool {
    spsr & (1 << 7) == 0
}

/// Nothing to report: the vectors run on the stack they interrupted.
pub(crate) fn report_fault_stack() {}

pub(crate) fn try_recover_from_panic() -> ! {
    owed!("recovering a syscall's panic", "stage 7")
}

pub fn kernel_exit_to_user_check() {
    owed!("the return to user mode", "stage 7")
}

pub(crate) fn log_unclaimed() {
    owed!("the interrupt controller", "stage 4")
}

#[cfg(feature = "test-actuators")]
pub(crate) fn provoke_double_fault() -> ! {
    owed!("a fault on the exception stack", "stage 4")
}
