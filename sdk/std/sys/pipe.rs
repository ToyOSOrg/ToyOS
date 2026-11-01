use toyos_abi::syscall;

use crate::io::{self, BorrowedCursor, IoSlice, IoSliceMut};
use crate::sys::{fd_error, new_fd, on_fd};

/// One end of a pipe: a descriptor naming it.
#[derive(Debug)]
pub struct Pipe {
    fd: i32,
}

pub fn pipe() -> io::Result<(Pipe, Pipe)> {
    let (read, write) = toyos::pipe_pair().map_err(crate::sys::to_io_error)?;
    let read = new_fd(read.into())?;
    Ok((Pipe { fd: read }, Pipe { fd: new_fd(write.into())? }))
}

impl Pipe {
    pub fn raw_fd(&self) -> i32 {
        self.fd
    }

    /// The kernel handle behind this end, held for `f`.
    pub fn with_handle<T>(&self, f: impl FnOnce(toyos::BorrowedHandle<'_>) -> T) -> io::Result<T> {
        on_fd(self.fd, |h| Ok(f(h)))
    }

    pub fn try_clone(&self) -> io::Result<Self> {
        toyos::fd::dup(self.fd, 0, false).map(|fd| Pipe { fd }).map_err(fd_error)
    }

    pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        on_fd(self.fd, |h| syscall::read(h.raw(), buf))
    }

    pub fn read_buf(&self, mut buf: BorrowedCursor<'_, u8>) -> io::Result<()> {
        let spare = buf.ensure_init();
        let n = self.read(spare)?;
        unsafe { buf.advance(n) };
        Ok(())
    }

    pub fn read_vectored(&self, bufs: &mut [IoSliceMut<'_>]) -> io::Result<usize> {
        match bufs.first_mut() {
            Some(b) => self.read(b),
            None => Ok(0),
        }
    }

    pub fn is_read_vectored(&self) -> bool {
        false
    }

    pub fn read_to_end(&self, buf: &mut Vec<u8>) -> io::Result<usize> {
        let mut total = 0;
        let mut tmp = [0u8; 4096];
        loop {
            let n = self.read(&mut tmp)?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            total += n;
        }
        Ok(total)
    }

    pub fn write(&self, buf: &[u8]) -> io::Result<usize> {
        on_fd(self.fd, |h| syscall::write(h.raw(), buf))
    }

    pub fn write_vectored(&self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        match bufs.first() {
            Some(b) => self.write(b),
            None => Ok(0),
        }
    }

    pub fn is_write_vectored(&self) -> bool {
        false
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        // SAFETY: the pipe owns its descriptor, and nothing names it after this.
        let _ = unsafe { toyos::fd::close(self.fd) };
    }
}
