//! A timer interrupt lands inside a syscall's body, and its fire re-arms one
//! quantum and not the span just armed.
//!
//! **A guest on the test kernel**, because whether the CPU takes an interrupt
//! inside a syscall exists only on a running gate, and nothing a guest does
//! puts one there on demand. `SYS_DEBUG`'s `RING0_TIMER_IN_SYSCALL` arms this
//! CPU's timer for 100 µs and waits inside the syscall for the kernel's own
//! fire; the gate, the fire and its re-arm are the shipped paths. Nothing is
//! timed: a gate that kept interrupts masked answers that no fire came within
//! the actuator's ceiling.

use toyos_abi::syscall::{self, debug_action};

fn main() {
    let answer = syscall::debug(debug_action::RING0_TIMER_IN_SYSCALL);
    assert_ne!(answer, debug_action::RING0_FIRE_NEVER, "no timer interrupt reached the syscall's body");
    assert_ne!(answer, debug_action::RING0_FIRE_OTHER_SPAN, "the fire inside the syscall re-armed another span than a quantum");
    assert_eq!(answer, debug_action::RING0_FIRE_REARMED, "an answer the actuator does not give");
    println!("ring0_timer_in_syscall: the timer interrupted the syscall's body and re-armed a quantum");
}
