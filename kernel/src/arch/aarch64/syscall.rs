//! The system-call gate is an `SVC` from EL0, taken through the vectors'
//! lower-EL synchronous entry (`super::trap`), which switches to `SP_EL1` in
//! hardware: there is no window with a user stack under the kernel.

/// A syscall entered this CPU; x86-64's NMI gate counts it, and AArch64 has no such gate.
pub fn note_entry() {}

/// x86-64's storms NMIs at its `SYSCALL` window, which AArch64 has none of.
pub fn window_storm() {
    panic!("syscall-window-nmi: AArch64's SVC switches stacks in hardware, so there is no window to storm");
}
