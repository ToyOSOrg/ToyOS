//! Vector 0xFF, the spurious vector `apic::enable_x2apic` writes into the SVR
//! on every CPU.
//!
//! The EOI is conditional: sent only when the ISR bit is set (SDM Vol. 3A
//! §11.9), since a genuine spurious interrupt sets none and an unconditional
//! EOI would strand some other in-service interrupt. Does not log — this
//! handler can run inside the log's own commit bracket.

use core::arch::naked_asm;

use crate::arch::apic;

/// The vector `apic::enable_x2apic` writes into the SVR; pub because the IDT
/// gate row hardcodes `0xFF` on its own and must be kept in sync by hand.
pub const SPURIOUS_VECTOR: u8 = 0xFF;

#[unsafe(naked)]
pub(super) extern "sysv64" fn spurious_entry() {
    naked_asm!(
        // Not routed through `ring3_naked_asm!`: this vector can interrupt any
        // instruction, `memmove`'s `std` … `cld` window included, so it owes
        // itself the `cld` every Ring 0 entry needs.
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

/// Counts one delivery and acknowledges it if the ISR bit shows it needed one.
extern "sysv64" fn took() {
    crate::irq_census::irq_took!(Spurious);
    if apic::in_service(SPURIOUS_VECTOR) {
        apic::eoi();
    }
}

/// Raises the spurious vector on this CPU and verifies the handler ran and
/// left the ISR clear.
#[cfg(feature = "boot-actuators")]
pub fn selftest() {
    use crate::irq_census::Source;

    let cpu = crate::arch::percpu::cpu_id();
    let Some(before) = crate::irq_census::deliveries(cpu, Source::Spurious) else {
        crate::log!("LAPIC: spurious selftest FAILED — this CPU publishes no census block");
        return;
    };
    let taken_before = crate::irq_census::deliveries_total(cpu).unwrap_or(0);

    // This platform's devices are all MSI/MSI-X and `TPR` is never written, so
    // nothing here raises a spurious interrupt without this call.
    apic::send_self(SPURIOUS_VECTOR);

    // A self-IPI arrives as soon as `IF` allows; the budget turns "never arrived" into a verdict.
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

    // A missing EOI leaves the ISR bit set, starving every vector below priority 0xF (SDM Vol. 3A §11.8.4).
    if apic::in_service(SPURIOUS_VECTOR) {
        crate::log!(
            "LAPIC: spurious selftest FAILED — vector {SPURIOUS_VECTOR:#x} is still in service, so \
             nothing below priority 0xF can be delivered on cpu{cpu} again"
        );
        return;
    }

    // Confirms the CPU still takes interrupts afterward; any source works, since only a deaf CPU is ruled out.
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
