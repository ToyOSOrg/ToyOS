//! The syscall ABI: [`dispatch`]'s argument-decode boundary and the
//! per-subsystem handlers. The entry that reaches it is the architecture's
//! (`arch::syscall`).

#[cfg(feature = "test-actuators")]
mod debug;
mod device;
pub(crate) mod dispatch;
mod event;
mod handles;
mod io;
mod ipc;
mod machine;
mod paged;
mod proc;
pub(crate) mod vm;

use toyos_abi::handle::ReceivedEntry;
use toyos_abi::syscall::SyscallError;

use crate::process;

/// Byte width of one handle a receive answers.
const RECEIVED_ENTRY_LEN: usize = core::mem::size_of::<ReceivedEntry>();

/// Byte width of one handle a send moves.
const TRANSFER_ENTRY_LEN: usize = core::mem::size_of::<toyos_abi::handle::TransferEntry>();

// Never read: `kernel_exit_to_user_check` sees the kill bit and exits before Ring 3.
fn cancelled() -> u64 {
    SyscallError::Gone.to_u64()
}

/// End the current userspace process for a fault nothing serves (called from exception handlers).
pub fn fault_process(fault: toyos_abi::end::Fault) -> ! {
    process::exit(toyos_abi::end::End::Fault(fault));
}
