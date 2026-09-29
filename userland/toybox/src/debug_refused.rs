//! `debug_refused`: a `SYS_DEBUG` action this machine has no counterpart for
//! is refused, and the kernel lives: the TLB acknowledgement delay, which
//! AArch64's broadcast invalidation has no acknowledgement for. Needs a
//! kernel built with `test-actuators`.

use toyos_abi::syscall::debug_action::{TLB_ACK_DELAY_ARM, TLB_ACK_DELAY_DISARM};
use toyos_abi::syscall::{debug, debug_with, SyscallError};

pub fn main(_args: Vec<String>) {
    let refused = SyscallError::NotSupported.to_u64();
    for (name, answer) in [
        ("TLB_ACK_DELAY_ARM", debug_with(TLB_ACK_DELAY_ARM, 1_000_000)),
        ("TLB_ACK_DELAY_DISARM", debug(TLB_ACK_DELAY_DISARM)),
    ] {
        assert_eq!(answer, refused, "SYS_DEBUG {name} answered {answer:#x}, not NotSupported");
    }
    println!("debug_refused: SYS_DEBUG's TLB acknowledgement delay was refused, twice");
}
