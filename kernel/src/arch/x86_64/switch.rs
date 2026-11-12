//! `context_switch`: the callee-saved registers a context stands on, pushed
//! onto the outgoing stack and popped off the incoming one.

use core::arch::naked_asm;
use core::sync::atomic::AtomicBool;

/// The outgoing half of [`context_switch`]. A macro, not inlined twice, so both builds share one instruction sequence.
///
/// The outgoing `rsp` is the last of the outgoing context written, so the
/// store that clears `left` after it may hand the context to another CPU. It
/// is a release: x86-64 does not reorder a write with older writes (SDM Vol.
/// 3A §11.2.2, order 325384-093US), and the caller's saves are ordinary
/// writes made before this call. Nothing pushes onto the outgoing stack after
/// the clear: interrupts are masked, and NMI, #MC, #DF and #DB switch to IST
/// stacks.
macro_rules! switch_save {
    () => {
        "pushfq
         push rbp
         push rbx
         push r12
         push r13
         push r14
         push r15
         mov [rdi], rsp
         test rdx, rdx
         jz 2f
         mov byte ptr [rdx], 0
         2:
         mov rsp, rsi"
    };
}

/// The incoming half: the seven words a resumed context stands on, ending in `ret`.
macro_rules! switch_restore {
    () => {
        "pop r15
         pop r14
         pop r13
         pop r12
         pop rbx
         pop rbp
         popfq
         ret"
    };
}

/// Callee-saved register save/restore. `left`, unless null, is the outgoing
/// task's on-CPU word, cleared once its context is saved.
#[cfg(not(feature = "switch-witness"))]
#[unsafe(naked)]
pub(crate) unsafe extern "C" fn context_switch(old_rsp: *mut u64, new_rsp: u64, left: *const AtomicBool) {
    naked_asm!(switch_save!(), switch_restore!());
}

/// The same switch with [`super::hw::switch_witness_verify`] between the stack move and the first `pop`; never fired.
///
/// Placed after `mov rsp, rsi`, reading the incoming frame through the register the machine will use. Sound
/// to `call`: the return lands inside the incoming task's own stack, and every register `verify` may clobber
/// is caller-saved and already dead here.
#[cfg(feature = "switch-witness")]
#[unsafe(naked)]
pub(crate) unsafe extern "C" fn context_switch(old_rsp: *mut u64, new_rsp: u64, left: *const AtomicBool) {
    naked_asm!(
        switch_save!(),
        "mov rdi, rsp",
        "call {verify}",
        switch_restore!(),
        verify = sym super::hw::switch_witness_verify,
    );
}
