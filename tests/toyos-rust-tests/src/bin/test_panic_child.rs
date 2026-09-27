//! Ask the kernel for one `SYS_DEBUG` action, and report what came back.
//!
//! Every action this is driven with ends the machine — so reaching the end at
//! all is a finding, and *which* finding is the whole of what it prints.
//! `InvalidArgument` is the kernel saying it has no debug syscall: the boot
//! needs `test-actuators` and asked for nothing, which is a harness mistake and
//! not a kernel that failed to stop.
//!
//! `NULL_READ` is given an address inside a live anonymous region this process
//! never touched: a page demand paging fills for Ring 3 and must not for Ring 0.

use toyos_abi::syscall::{self, debug_action, MmapFlags, MmapProt, SyscallError};

const PAGE_2M: usize = 2 << 20;

fn main() {
    let action: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .expect("usage: test_panic_child <SYS_DEBUG action>");
    let rc = if action == debug_action::NULL_READ {
        // SAFETY: a fresh anonymous region this process never dereferences.
        let base = unsafe {
            syscall::mmap(
                core::ptr::null_mut(),
                2 * PAGE_2M,
                MmapProt::READ | MmapProt::WRITE,
                MmapFlags::ANONYMOUS | MmapFlags::PRIVATE,
            )
        };
        assert!(!base.is_null(), "mmap of {} bytes failed", 2 * PAGE_2M);
        syscall::debug_with(action, base as u64 + PAGE_2M as u64)
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
