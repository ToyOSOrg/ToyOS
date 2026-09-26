//! The system-call gate: `SVC` from EL0 through the vectors' lower-EL entry,
//! the port's stage 7.


/// A syscall entered this CPU; x86-64's NMI gate counts it, and AArch64 has no
/// such gate.
pub fn note_entry() {}

/// The actuator that storms NMIs at the syscall window.
pub fn window_storm() {
    owed!("the syscall gate", "stage 7")
}
