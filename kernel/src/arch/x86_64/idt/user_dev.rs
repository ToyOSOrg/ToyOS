//! The vectors a PCI function a *process* drives delivers on: one taken by
//! each claim and given back at its release, from cpu0's vectors this kernel
//! declares nothing on.
//!
//! One per claim, because the vector is what the kernel knows a message by:
//! the count goes to that function's own record, and the wake it earns goes to
//! its watchers and nobody else's. A single shared vector would wake every
//! user driver in the machine on any of their interrupts, which is one process
//! learning when another's device is busy.
//!
//! Heap-free like every other device entry here: each vector's stub pushes its
//! number, the record is atomics, and the claim's watch is posted from the
//! handler.

use kernel::owners::Owners;

use super::device_irq::device_irq_entry;
use super::{Idt, IdtEntry};

/// The claims' vectors, from here up to the IPIs at 0xFC: `idt_vectors!`
/// declares its device vectors below.
const FIRST: u8 = 0x40;
const COUNT: usize = 0xFC - FIRST as usize;

/// Each stub's bytes: a push and a jump fit in 10.
const STRIDE: usize = 16;

/// The `pcidev` function each vector is held for.
static HELD: Owners<COUNT> = Owners::new();

/// A claim's vector, held until [`give_back`].
#[derive(Clone, Copy)]
pub(crate) struct ClaimVector(u8);

impl ClaimVector {
    pub(crate) fn vector(self) -> u8 {
        self.0
    }
}

impl core::fmt::Display for ClaimVector {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "vector {:#04x}", self.0)
    }
}

/// A vector for `pcidev` function `function`, or `None` once every one is held.
pub(crate) fn take(function: usize) -> Option<ClaimVector> {
    let owner = u32::try_from(function).expect("a function number fits a u32");
    HELD.take(owner).map(|number| ClaimVector(FIRST + number as u8))
}

/// Free `vector` again, once nothing can raise it for the claim it was taken for.
pub(crate) fn give_back(vector: ClaimVector) {
    HELD.give_back(usize::from(vector.0 - FIRST));
}

/// Every claim vector's gate onto its stub.
pub(super) fn install(idt: &mut Idt) {
    let first = (stubs as *const () as usize).next_multiple_of(STRIDE);
    for number in 0..COUNT {
        let entry = &mut idt.entries[usize::from(FIRST) + number];
        assert!(entry.type_attr == 0, "idt: vector {:#04x} is declared and a claim vector", usize::from(FIRST) + number);
        *entry = IdtEntry::at_address((first + number * STRIDE) as u64);
    }
}

/// `COUNT` stubs, `STRIDE` apart from the first `STRIDE` boundary: each pushes
/// its number and jumps to [`claim_entry`]. Never called.
#[unsafe(naked)]
extern "sysv64" fn stubs() {
    core::arch::naked_asm!(
        ".set toyos_claim_vector, 0",
        ".rept {count}",
        ".balign {stride}",
        "pushq $toyos_claim_vector",
        "jmp {entry}",
        ".set toyos_claim_vector, toyos_claim_vector + 1",
        ".endr",
        count = const COUNT,
        stride = const STRIDE,
        entry = sym claim_entry,
        options(att_syntax),
    );
}

extern "sysv64" fn took(number: u64) {
    match HELD.owner(number as usize) {
        Some(function) => {
            crate::arch::percpu::irq_took!(UserDev);
            crate::pcidev::isr(function as usize);
            crate::preempt::set_need_resched();
        }
        // A message its claim's release had not yet stopped before the vector
        // was given back.
        None => super::unclaimed::count(FIRST + number as u8),
    }
    crate::arch::apic::eoi();
}

device_irq_entry! {
    /// Every claim vector's stub ends here (see `device_irq_entry` for the asm contract).
    fn claim_entry => took, numbered
}
