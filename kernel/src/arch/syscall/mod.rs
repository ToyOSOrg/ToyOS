//! The syscall ABI: the gate the CPU enters through, the decode that turns four
//! register words into typed arguments, and the handlers those arguments reach.
//!
//! It was one file holding three unrelated things — the entry asm, the ABI
//! register mapping, and every handler — and the split is by what a reader is
//! looking for:
//!
//! - [`gate`] — the three MSRs `SYSCALL` is aimed by and the naked entry it
//!   lands on. The machine half, and nothing in it knows a syscall number.
//! - [`dispatch`] — **the user/kernel argument boundary**, and the file to
//!   audit when a bug class is about what userland named. Read its header
//!   before adding an arm.
//! - the handlers, by subsystem: [`fs`], [`io`], [`ipc`], [`proc`], [`vm`],
//!   [`device`], [`machine`], with the handle table's own shapes in
//!   [`handles`] and `SYS_DEBUG`'s state in `debug`.
//!
//! Two things are shared by more than one of them and live here: the width of a
//! handle on the wire, and the word a cancelled wait answers in.

#[cfg(feature = "test-actuators")]
mod debug;
mod device;
mod dispatch;
mod fs;
mod gate;
mod handles;
mod io;
mod ipc;
mod machine;
mod proc;
mod vm;

pub use gate::init;

use toyos_abi::handle::RawHandle;
use toyos_abi::syscall::SyscallError;

use crate::process;

/// One [`RawHandle`] on the wire, for a vector of them a syscall reads out of
/// user memory a handle at a time.
const HANDLE_LEN: usize = core::mem::size_of::<RawHandle>();

/// What a syscall answers when its wait was cancelled.
///
/// **Nothing ever reads it.** The thread has been killed, so the return path
/// it is on ends at `kernel_exit_to_user_check`, which sees the kill bit and
/// exits instead of returning to Ring 3. The word exists because the
/// unwind has to carry *something* through the `u64` every syscall answers in,
/// and `Interrupted` is what it would mean if anything could read it.
fn cancelled() -> u64 {
    SyscallError::Gone.to_u64()
}

/// Terminate the current userspace process (called from exception handlers).
pub fn kill_process(code: i32) -> ! {
    process::exit(code);
}
