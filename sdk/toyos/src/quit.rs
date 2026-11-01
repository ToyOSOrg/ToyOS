//! What this process is asked to quit for.
//!
//! A holder of `MANAGE` on a process asks it and everything below it to quit
//! ([`crate::process::Process::quit`]). Each process is started holding its
//! notice, where the reason waits until it is read. **A process that never
//! listened is killed instead**, and so is one whose notice still holds the
//! reason it was last asked, which is what a second Ctrl+C does to a program
//! that has not taken the first. A process listens from its first read of the
//! notice or the first readable watch armed on it, and for good.

pub use toyos_abi::quit::Reason;
use toyos_abi::syscall::{self, SyscallError};

use crate::endow::{FromHandle, Once};
use crate::{AsHandle, OwnedHandle, RawHandle};

/// This process's notice.
pub struct Notice(OwnedHandle);

impl Notice {
    /// Listen from now on, taking nothing.
    pub fn listen(&self) -> Result<(), SyscallError> {
        syscall::read_nonblock(self.0.raw(), &mut []).map(drop)
    }

    /// The reason asked, waiting for one; listens from now on.
    pub fn read(&self) -> Result<Reason, SyscallError> {
        let mut byte = [0u8];
        syscall::read(self.0.raw(), &mut byte)?;
        Ok(reason(byte[0]))
    }

    /// The reason asked, or `None`; listens from now on.
    pub fn try_read(&self) -> Result<Option<Reason>, SyscallError> {
        let mut byte = [0u8];
        match syscall::read_nonblock(self.0.raw(), &mut byte) {
            Ok(_) => Ok(Some(reason(byte[0]))),
            Err(SyscallError::WouldBlock) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// The reason a read answered, which is one the kernel makes or a kernel bug.
fn reason(byte: u8) -> Reason {
    Reason::from_byte(byte).unwrap_or_else(|| panic!("a notice's read answered {byte}, which names no reason"))
}

impl AsHandle for Notice {
    fn as_handle(&self) -> crate::BorrowedHandle<'_> {
        self.0.as_handle()
    }
}

impl FromHandle for Notice {
    fn from_handle(handle: OwnedHandle) -> Self {
        Self(handle)
    }
}

/// This process's notice, from the kernel under `QUIT_LABEL`. Borrowed, as
/// [`crate::endow::this_process`] is.
///
/// **One copy of this crate takes it**, the one that owns the descriptor
/// table (`crate::fd`): std's in a Rust program, libc's own in a C program.
/// Every copy asks that one, so a Rust program that links libc listens to one
/// notice whichever copy reads it.
pub fn notice() -> Option<&'static Notice> {
    static NOTICE: Once<Option<Notice>> = Once::new();
    NOTICE
        .get_or_init(|| {
            // SAFETY: the owner's definition below, which answers a handle the
            // process holds for good, or none.
            let raw = RawHandle(unsafe { toyos_quit_notice() });
            (raw != toyos_abi::HANDLE_INVALID).then_some(Notice(OwnedHandle(raw)))
        })
        .as_ref()
}

extern "C" {
    fn toyos_quit_notice() -> u32;
}

/// The copy that owns the notice.
#[cfg(all(feature = "fd-table", not(test)))]
mod owner {
    use super::Notice;
    use crate::endow::{Endowments, Once};

    /// The notice, taken once from this process's endowments and never
    /// closed, or `HANDLE_INVALID` for none.
    #[no_mangle]
    extern "C" fn toyos_quit_notice() -> u32 {
        static TAKEN: Once<u32> = Once::new();
        *TAKEN.get_or_init(|| {
            Endowments::get()
                .take::<Notice>(toyos_abi::syscall::QUIT_LABEL)
                .map_or(toyos_abi::HANDLE_INVALID.0, |notice| notice.0.into_raw().0)
        })
    }
}
