//! Vector 0xFF, which this kernel names by writing it into the SVR.
//!
//! `apic::enable_x2apic` puts `0xFF` in the spurious-interrupt vector field on
//! the BSP and on every AP, so the local APIC will deliver that vector whenever
//! it takes back an interrupt it had already signalled. A vector the CPU can
//! deliver and the IDT leaves `P = 0` is not a fault: the missing gate is a
//! second, contributory fault and the CPU escalates to `#DF`, which
//! `double_fault_handler` answers by halting the machine. That is the rule
//! `idt_vectors!`' own comment states for the exception range, and this is the
//! one vector the platform — rather than Intel — names.
//!
//! **The EOI is conditional, and that is the whole of the handler's
//! difficulty.** A genuine spurious interrupt sets no ISR bit (SDM Vol. 3A
//! §11.9), so an unconditional `eoi()` here would clear some *other*
//! interrupt's in-service bit and lose it. The same vector reached by a
//! deliberate IPI does go through the IRR and does need one — and without it
//! the ISR bit at priority 0xF blocks every lower-priority vector on this CPU
//! for the rest of the boot, the timer included. So the handler asks the ISR
//! which of the two it is.
//!
//! **It does not log**, for `idt::nmi`'s reason: it can arrive inside the log's
//! own commit bracket. It records one delivery in the interrupt census, which
//! is a single `add` to this CPU's own counter block and reaches no lock.

use core::arch::naked_asm;

use crate::arch::apic;

/// The vector `apic::enable_x2apic` writes into the SVR. Public because the
/// gate's row and the register write are two places, and only one may decide.
pub const SPURIOUS_VECTOR: u8 = 0xFF;

#[unsafe(naked)]
pub(super) extern "sysv64" fn spurious_entry() {
    naked_asm!(
        // The `cld` every Ring 0 entry owes itself (`arch::entry`), at a gate
        // that is not routed through `ring3_naked_asm!`: this vector can arrive
        // between any two instructions, `memmove`'s `std` … `cld` window
        // included, and `took` is a `sysv64` call.
        "cld",
        "push rax",
        "push rcx",
        "push rdx",
        "push rsi",
        "push rdi",
        "push r8",
        "push r9",
        "push r10",
        "push r11",
        "push rbp",
        "mov rbp, rsp",
        "and rsp, -16",
        "call {took}",
        "mov rsp, rbp",
        "pop rbp",
        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rdi",
        "pop rsi",
        "pop rdx",
        "pop rcx",
        "pop rax",
        "iretq",
        took = sym took,
    );
}

/// One delivery of the spurious vector, counted and acknowledged if it is one
/// of the deliveries that can be.
extern "sysv64" fn took() {
    crate::irq_census::irq_took!(Spurious);
    if apic::in_service(SPURIOUS_VECTOR) {
        apic::eoi();
    }
}

/// Raise the spurious vector on this CPU on purpose, and check the three things
/// a boot cannot otherwise certify: that the machine survives the delivery,
/// that the handler ran, and that the interrupt after it is not lost.
///
/// **Nothing on this host produces a genuine spurious interrupt.** The SDM's
/// classic condition is an interrupt masked by a task-priority register raised
/// between assertion and `INTA`, and this kernel never writes `TPR`; every
/// device on the machine is MSI or MSI-X. So without this the gate would ship
/// never having been entered — and the third assertion is the one that matters,
/// because a handler that acknowledged nothing would leave an ISR bit set at
/// priority 0xF and starve every vector below it, the timer included.
#[cfg(feature = "boot-actuators")]
pub fn selftest() {
    use crate::irq_census::Source;

    let cpu = crate::arch::percpu::cpu_id();
    let Some(before) = crate::irq_census::deliveries(cpu, Source::Spurious) else {
        crate::log!("LAPIC: spurious selftest FAILED — this CPU publishes no census block");
        return;
    };
    let taken_before = crate::irq_census::deliveries_total(cpu).unwrap_or(0);

    apic::send_self(SPURIOUS_VECTOR);

    // A self-IPI is delivered as soon as `IF` allows, which is now; the budget
    // is what turns "it never arrived" into a verdict instead of a hang.
    const ARRIVES: crate::time::Budget = crate::time::Budget::of(
        crate::time::Duration::from_millis(50),
        "the delivery is reported as never having arrived",
    );
    let delivered = crate::clock::settles(ARRIVES.nanos(), || {
        crate::irq_census::deliveries(cpu, Source::Spurious).unwrap_or(before) > before
    });
    if !delivered {
        crate::log!("LAPIC: spurious selftest FAILED — vector {SPURIOUS_VECTOR:#x} never arrived");
        return;
    }

    // Acknowledged. A missing EOI does not fault: it leaves the bit set, this
    // CPU's in-service priority at 0xF, and every lower vector — the timer
    // included — undeliverable for the rest of the boot (SDM Vol. 3A §11.8.4).
    if apic::in_service(SPURIOUS_VECTOR) {
        crate::log!(
            "LAPIC: spurious selftest FAILED — vector {SPURIOUS_VECTOR:#x} is still in service, so \
             nothing below priority 0xF can be delivered on cpu{cpu} again"
        );
        return;
    }

    // …and the machine takes interrupts after it, which is that argument's
    // observable half. Any source will do: what is being ruled out is a CPU
    // that has gone deaf, not a particular device.
    let ran_on = crate::clock::settles(ARRIVES.nanos(), || {
        crate::irq_census::deliveries_total(cpu).unwrap_or(taken_before) > taken_before
    });
    if !ran_on {
        crate::log!(
            "LAPIC: spurious selftest FAILED — cpu{cpu} took no interrupt at all after the \
             spurious one"
        );
        return;
    }

    let after = crate::irq_census::deliveries(cpu, Source::Spurious).unwrap_or(before);
    crate::log!(
        "LAPIC: spurious selftest 3/3 — vector {:#x} delivered on cpu{} ({} -> {}), acknowledged, \
         and the CPU took interrupts after it",
        SPURIOUS_VECTOR,
        cpu,
        before,
        after,
    );
}
