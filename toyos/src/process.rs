//! A process, as a thing you hold rather than a number you know.
//!
//! A pid names a process the way a street name names a house: everyone can say
//! it and saying it is not a key. `SYS_SPAWN` answers with one of these, the
//! launcher sends one back, and there is no other way to get one — so what may
//! wait for a process, kill it or read its accounting is exactly what was given
//! a handle to it.

use toyos_abi::handle::Rights;
use toyos_abi::syscall::{self, ProcessStats, SyscallError};

use crate::endow::FromHandle;
use crate::shm::SharedMemory;
use crate::{AsHandle, OwnedHandle, RawHandle};

pub struct Process(pub(crate) OwnedHandle);

impl Process {
    /// Block until it exits, and take the code.
    ///
    /// **Repeatable and never missed.** The code is on the object, so waiting a
    /// second time answers the same thing and waiting long after the process is
    /// gone still answers.
    pub fn wait(&self) -> Result<i32, SyscallError> {
        syscall::process_wait(self.0.raw())
    }

    /// The exit code if it has already exited, `Err(WouldBlock)` if not.
    pub fn try_wait(&self) -> Result<i32, SyscallError> {
        syscall::process_wait_nonblock(self.0.raw())
    }

    /// Kill it. `Ok` for one already dead: the caller asked for it to be gone.
    pub fn kill(&self) -> Result<(), SyscallError> {
        syscall::process_kill(self.0.raw())
    }

    pub fn stats(&self) -> Result<ProcessStats, SyscallError> {
        let mut stats = ProcessStats::default();
        syscall::process_stats(self.0.raw(), &mut stats)?;
        Ok(stats)
    }

    /// A second handle carrying **less** — how a supervisor hands on the right
    /// to wait without the right to kill.
    pub fn narrowed(&self, rights: Rights) -> Result<Self, SyscallError> {
        syscall::dup_narrowed(self.0.raw(), rights).map(|h| Self(OwnedHandle(h)))
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
    fn as_handle(&self) -> RawHandle {
        self.0.raw()
    }
}

impl FromHandle for Process {
    unsafe fn from_handle(raw: RawHandle) -> Self {
        Self(OwnedHandle(raw))
    }
}

/// A program's bytes in a memory object of this process's own, which the
/// kernel pages a child from: how a program the kernel does not serve is
/// spawned (`SpawnArgs::image`).
pub struct Image {
    object: SharedMemory,
    len: u64,
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
    pub fn read<E>(
        len: u64,
        mut read: impl FnMut(&mut [u8]) -> Result<usize, E>,
    ) -> Result<Self, ImageRefused<E>> {
        let size = usize::try_from(len).map_err(|_| ImageRefused::Memory(SyscallError::InvalidArgument))?;
        if size == 0 {
            return Err(ImageRefused::Empty);
        }
        let mut object = SharedMemory::create(size).map_err(ImageRefused::Memory)?;
        let bytes = object.as_mut_slice();
        let mut done = 0;
        while done < size {
            match read(&mut bytes[done..]).map_err(ImageRefused::Read)? {
                0 => return Err(ImageRefused::Shrank),
                n => done += n,
            }
        }
        Ok(Self { object, len })
    }

    /// The program's bytes, as they were read.
    pub fn bytes(&self) -> &[u8] {
        &self.object.as_slice()[..self.len as usize]
    }

    /// What `SpawnArgs::image` and `SpawnArgs::image_len` carry. The object is
    /// this process's until the spawn returns; the child keeps its own.
    pub fn spawn_words(&self) -> (u64, u64) {
        (u64::from(self.object.as_handle().0), self.len)
    }
}
