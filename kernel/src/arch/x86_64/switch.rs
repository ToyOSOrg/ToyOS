//! `context_switch`: the callee-saved registers a context stands on, pushed
//! onto the outgoing stack and popped off the incoming one.

use core::arch::naked_asm;

/// The outgoing half of [`context_switch`]. A macro, not inlined twice, so both builds share one instruction sequence.
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

/// Callee-saved register save/restore.
#[cfg(not(feature = "switch-witness"))]
#[unsafe(naked)]
pub(crate) unsafe extern "C" fn context_switch(old_rsp: *mut u64, new_rsp: u64) {
    naked_asm!(switch_save!(), switch_restore!());
}

/// The same switch with [`super::hw::switch_witness_verify`] between the stack move and the first `pop`; never fired.
///
/// Placed after `mov rsp, rsi`, reading the incoming frame through the register the machine will use. Sound
/// to `call`: the return lands inside the incoming task's own stack, and every register `verify` may clobber
/// is caller-saved and already dead here.
#[cfg(feature = "switch-witness")]
#[unsafe(naked)]
pub(crate) unsafe extern "C" fn context_switch(old_rsp: *mut u64, new_rsp: u64) {
    naked_asm!(
        switch_save!(),
        "mov rdi, rsp",
        "call {verify}",
        switch_restore!(),
        verify = sym super::hw::switch_witness_verify,
    );
}
