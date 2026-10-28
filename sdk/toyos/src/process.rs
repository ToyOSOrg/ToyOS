//! A process, as a thing you hold rather than a number you know.
//!
//! A pid names a process the way a street name names a house: everyone can say
//! it and saying it is not a key. `SYS_SPAWN` answers with one of these, the
//! launcher sends one back, and there is no other way to get one — so what may
//! wait for a process, kill it or read its accounting is exactly what was given
//! a handle to it.

pub use toyos_abi::end::{End, Fault};
use toyos_abi::handle::Rights;
use toyos_abi::syscall::{self, ProcessStats, SyscallError};

use crate::endow::FromHandle;
use crate::shm::{Immutable, SharedMemory};
use crate::{AsHandle, OwnedHandle, RawHandle};

/// The `<signal.h>` number POSIX names an end by, as libc's `WTERMSIG`
/// answers it and a shell's `$?` adds to 128: a kill is `SIGKILL`, a fault its
/// class's signal, and a handle fault `SIGSEGV`'s, a pointer being the other way
/// to name what a process does not hold. `None` for an exit.
pub fn signal(end: End) -> Option<i32> {
    const SIGILL: i32 = 4;
    const SIGTRAP: i32 = 5;
    const SIGBUS: i32 = 7;
    const SIGFPE: i32 = 8;
    const SIGKILL: i32 = 9;
    const SIGSEGV: i32 = 11;
    Some(match end {
        End::Exited(_) => return None,
        End::Killed => SIGKILL,
        End::Fault(Fault::Memory | Fault::Handle) => SIGSEGV,
        End::Fault(Fault::Alignment) => SIGBUS,
        End::Fault(Fault::Instruction) => SIGILL,
        End::Fault(Fault::Arithmetic) => SIGFPE,
        End::Fault(Fault::Trap) => SIGTRAP,
    })
}

pub struct Process(pub(crate) OwnedHandle);

impl Process {
    /// Block until it ends, and take how.
    ///
    /// **Repeatable and never missed.** The end is on the object, so waiting a
    /// second time answers the same thing and waiting long after the process is
    /// gone still answers.
    pub fn wait(&self) -> Result<End, SyscallError> {
        syscall::process_wait(self.0.raw())
    }

    /// How it ended if it has, `Err(WouldBlock)` if not.
    pub fn try_wait(&self) -> Result<End, SyscallError> {
        syscall::process_wait_nonblock(self.0.raw())
    }

    /// Kill it. `Ok` for one already dead: the caller asked for it to be gone.
    pub fn kill(&self) -> Result<(), SyscallError> {
        syscall::process_kill(self.0.raw())
    }

    /// Ask it, and everything below it, to quit for `reason` ([`crate::quit`]).
    /// `Ok` for one already ended, as [`Self::kill`] answers.
    pub fn quit(&self, reason: crate::quit::Reason) -> Result<(), SyscallError> {
        syscall::process_quit(self.0.raw(), reason)
    }

    pub fn stats(&self) -> Result<ProcessStats, SyscallError> {
        let mut stats = ProcessStats::default();
        syscall::process_stats(self.0.raw(), &mut stats)?;
        Ok(stats)
    }

    /// A second handle carrying **less** — how a supervisor hands on the right
    /// to wait without the right to kill.
    pub fn narrowed(&self, rights: Rights) -> Result<Self, SyscallError> {
        self.0.as_handle().try_clone_narrowed(rights).map(Self)
    }

    /// Give up ownership, for a handle about to be endowed or sent.
    pub fn into_raw(self) -> RawHandle {
        self.0.into_raw()
    }

    /// # Safety
    /// `raw` must be a live process handle this process owns and nothing else
    /// answers for.
    pub unsafe fn from_raw(raw: RawHandle) -> Self {
        Self(OwnedHandle(raw))
    }
}

impl AsHandle for Process {
    fn as_handle(&self) -> crate::BorrowedHandle<'_> {
        self.0.as_handle()
    }
}

impl FromHandle for Process {
    fn from_handle(handle: OwnedHandle) -> Self {
        Self(handle)
    }
}

/// A program's bytes in a memory object this process holds, which the kernel
/// pages a child from: how a program the kernel does not serve is spawned
/// (`SpawnArgs::image`).
pub struct Image {
    object: Object,
    len: u64,
}

enum Object {
    /// Read into a region of this process's own, which this mapping alone
    /// writes: its handle leaves only for a spawn, whose kernel copies each
    /// page and writes none.
    Read(SharedMemory),
    /// A region nobody writes: a boot image, or a file its server holds so.
    Immutable(Immutable),
}

/// Why [`Image::read`] made no image.
#[derive(Debug)]
pub enum ImageRefused<E> {
    /// The program is an empty file.
    Empty,
    /// No memory object would hold it.
    Memory(SyscallError),
    /// The reader ran out before the length it was read at.
    Shrank,
    /// The reader refused.
    Read(E),
}

impl Image {
    /// `len` bytes, from `read` asked until it has given them all.
    pub fn read<E>(len: u64, read: impl FnMut(&mut [u8]) -> Result<usize, E>) -> Result<Self, ImageRefused<E>> {
        Self::read_for(None, len, read)
    }

    /// [`read`](Self::read), counted to the job `job` is a handle to, rather
    /// than this process's: the program of a child started in it, which
    /// keeps the image for as long as it runs.
    pub fn read_for<E>(
        job: Option<crate::BorrowedHandle<'_>>,
        len: u64,
        mut read: impl FnMut(&mut [u8]) -> Result<usize, E>,
    ) -> Result<Self, ImageRefused<E>> {
        let size = usize::try_from(len).map_err(|_| ImageRefused::Memory(SyscallError::InvalidArgument))?;
        if size == 0 {
            return Err(ImageRefused::Empty);
        }
        let created = match job {
            Some(job) => SharedMemory::create_for(job, size),
            None => SharedMemory::create(size),
        };
        let object = created.map_err(ImageRefused::Memory)?;
        // SAFETY: `size` bytes of a fresh mapping no handle has left this
        // process for, borrowed while `object` lives here.
        let bytes = unsafe { core::slice::from_raw_parts_mut(object.as_ptr(), size) };
        let mut done = 0;
        while done < size {
            match read(&mut bytes[done..]).map_err(ImageRefused::Read)? {
                0 => return Err(ImageRefused::Shrank),
                n => done += n,
            }
        }
        Ok(Self { object: Object::Read(object), len })
    }

    /// The program a region nobody writes is, whole.
    pub fn immutable(image: Immutable) -> Self {
        let len = image.as_slice().len() as u64;
        Self { object: Object::Immutable(image), len }
    }

    /// The program's bytes, as they were read.
    pub fn bytes(&self) -> &[u8] {
        let bytes = match &self.object {
            // SAFETY: the mapping is `object.len()` bytes and lives as long as
            // `self`, and nothing writes them once read (`Object::Read`).
            Object::Read(object) => unsafe { core::slice::from_raw_parts(object.as_ptr(), object.len()) },
            Object::Immutable(image) => image.as_slice(),
        };
        &bytes[..self.len as usize]
    }

    /// What `SpawnArgs::image` and `SpawnArgs::image_len` carry. The object is
    /// this process's until the spawn returns; the child keeps its own.
    pub fn spawn_words(&self) -> (u64, u64) {
        let handle = match &self.object {
            Object::Read(object) => object.as_handle(),
            Object::Immutable(image) => image.as_handle(),
        };
        (u64::from(handle.raw().0), self.len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The number `<signal.h>` defines `name` as.
    fn defined(name: &str) -> i32 {
        let header = include_str!("../../libc/include/signal.h");
        let line = header.lines().find(|l| l.split_whitespace().nth(1) == Some(name)).expect(name);
        line.split_whitespace().nth(2).and_then(|n| n.parse().ok()).expect(name)
    }

    /// Each end is the signal libc's header names its class by, and an exit
    /// is none, 137 included.
    #[test]
    fn each_end_is_its_classes_signal() {
        for (end, name) in [
            (End::Killed, "SIGKILL"),
            (End::Fault(Fault::Memory), "SIGSEGV"),
            (End::Fault(Fault::Handle), "SIGSEGV"),
            (End::Fault(Fault::Alignment), "SIGBUS"),
            (End::Fault(Fault::Instruction), "SIGILL"),
            (End::Fault(Fault::Arithmetic), "SIGFPE"),
            (End::Fault(Fault::Trap), "SIGTRAP"),
        ] {
            assert_eq!(signal(end), Some(defined(name)), "{end:?}");
        }
        assert_eq!(signal(End::Exited(137)), None);
    }
}
