//! Ask the kernel for one `SYS_DEBUG` action, and report what came back.
//!
//! Every action this is driven with ends the machine — so reaching the end at
//! all is a finding, and *which* finding is the whole of what it prints.
//! `InvalidArgument` is the kernel saying it has no debug syscall: the boot
//! needs `test-actuators` and asked for nothing, which is a harness mistake and
//! not a kernel that failed to stop.
//!
//! `NULL_READ` is given a 2 MiB window wholly inside [`UNTOUCHED`]: a page
//! demand paging fills for Ring 3 and must not for Ring 0. `.bss` and not an
//! `mmap`, because an anonymous `mmap` is mapped when it is made.

use toyos_abi::syscall::{self, debug_action, SyscallError};

const PAGE_2M: usize = 2 << 20;

/// `.bss` this process never touches. Twice the window, so one 2 MiB-aligned
/// window lies inside it wherever the loader placed it.
static mut UNTOUCHED: [u8; 2 * PAGE_2M] = [0; 2 * PAGE_2M];

fn main() {
    let action: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .expect("usage: test_panic_child <SYS_DEBUG action>");
    let rc = if action == debug_action::NULL_READ {
        let window = (&raw const UNTOUCHED as usize).next_multiple_of(PAGE_2M);
        syscall::debug_with(action, window as u64)
    } else {
        syscall::debug(action)
    };
    if rc == SyscallError::InvalidArgument.to_u64() {
        eprintln!(
            "ERROR: SYS_DEBUG {action} answered InvalidArgument — this kernel carries no \
             actuators, so the boot that drives this one needs `test-actuators`"
        );
    } else {
        eprintln!("ERROR: SYS_DEBUG {action} returned {rc:#x}, the kernel did not end the machine");
    }
    std::process::exit(0);
}
