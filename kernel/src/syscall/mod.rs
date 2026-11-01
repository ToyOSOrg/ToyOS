//! The syscall ABI: [`dispatch`]'s argument-decode boundary and the
//! per-subsystem handlers. The entry that reaches it is the architecture's
//! (`arch::syscall`).

#[cfg(feature = "test-actuators")]
mod debug;
mod device;
pub(crate) mod dispatch;
mod event;
mod fs;
mod handles;
mod io;
mod ipc;
mod machine;
mod paged;
mod proc;
mod vm;

use toyos_abi::handle::RawHandle;
use toyos_abi::syscall::SyscallError;

use crate::process;

/// Byte width of one [`RawHandle`] on the wire.
const HANDLE_LEN: usize = core::mem::size_of::<RawHandle>();

// Never read: `kernel_exit_to_user_check` sees the kill bit and exits before Ring 3.
fn cancelled() -> u64 {
    SyscallError::Gone.to_u64()
}

/// End the current userspace process for a fault nothing serves (called from exception handlers).
pub fn fault_process(fault: toyos_abi::end::Fault) -> ! {
    process::exit(toyos_abi::end::End::Fault(fault));
}
