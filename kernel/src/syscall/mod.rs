//! The syscall ABI: [`dispatch`]'s argument-decode boundary and the
//! per-subsystem handlers. The entry that reaches it is the architecture's
//! (`arch::syscall`).

#[cfg(feature = "test-actuators")]
mod debug;
mod device;
pub(crate) mod dispatch;
mod fs;
mod handles;
mod io;
mod ipc;
mod machine;
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

/// Terminate the current userspace process (called from exception handlers).
pub fn kill_process(code: i32) -> ! {
    process::exit(code);
}
