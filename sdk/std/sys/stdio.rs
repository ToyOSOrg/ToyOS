use core::sync::atomic::{AtomicBool, Ordering};

use toyos::log::stdio::{self as log_stdio, Stream};
use toyos_abi::RawHandle;
use toyos_abi::syscall::{self, FileType};

use crate::io::{self, IoSlice, IoSliceMut};
use crate::sys::to_io_error;

const STDIN: RawHandle = RawHandle(0);

// ---------------------------------------------------------------------------
// Stdin mode flag (canonical by default, raw when explicitly switched)
// ---------------------------------------------------------------------------

static STDIN_RAW: AtomicBool = AtomicBool::new(false);

pub fn set_stdin_raw(raw: bool) {
    STDIN_RAW.store(raw, Ordering::Relaxed);
}

// ---------------------------------------------------------------------------
// Canonical line buffer
// ---------------------------------------------------------------------------

const LINE_BUF_CAP: usize = 256;

struct LineBuf {
    buf: [u8; LINE_BUF_CAP],
    len: usize,
    pos: usize,
}

// Safety: Stdin::read() is always called under the global Stdin mutex and ToyOS
// is single-threaded, so there is no concurrent access.
static mut LINE_BUF: LineBuf = LineBuf { buf: [0; LINE_BUF_CAP], len: 0, pos: 0 };

fn read_one() -> io::Result<u8> {
    let mut byte = [0u8; 1];
    let n = syscall::read(STDIN, &mut byte).map_err(to_io_error)?;
    if n == 0 { Err(io::Error::new(io::ErrorKind::UnexpectedEof, "eof")) } else { Ok(byte[0]) }
}

fn echo(bytes: &[u8]) {
    let _ = log_stdio::write(Stream::Out, bytes);
}

/// Canonical read: line editing with echo. Buffers a complete line, then
/// serves bytes from the buffer on subsequent calls.
fn canonical_read(buf: &mut [u8]) -> io::Result<usize> {
    // Safety: see LINE_BUF comment above.
    let lb = unsafe { &mut *core::ptr::addr_of_mut!(LINE_BUF) };

    // Serve remaining data from a previous line first.
    if lb.pos < lb.len {
        let avail = lb.len - lb.pos;
        let n = avail.min(buf.len());
        buf[..n].copy_from_slice(&lb.buf[lb.pos..lb.pos + n]);
        lb.pos += n;
        return Ok(n);
    }

    // Read a new line with echo + backspace handling.
    lb.len = 0;
    lb.pos = 0;

    loop {
        let ch = read_one()?;
        match ch {
            b'\r' | b'\n' => {
                // Translate CR to LF (like Unix terminal driver ICRNL)
                echo(b"\n");
                if lb.len < LINE_BUF_CAP {
                    lb.buf[lb.len] = b'\n';
                    lb.len += 1;
                }
                let n = lb.len.min(buf.len());
                buf[..n].copy_from_slice(&lb.buf[..n]);
                lb.pos = n;
                return Ok(n);
            }
            0x08 | 0x7F => {
                // Backspace: erase last UTF-8 character
                if lb.len > 0 {
                    // Scan past continuation bytes (10xxxxxx)
                    lb.len -= 1;
                    while lb.len > 0 && (lb.buf[lb.len] & 0xC0) == 0x80 {
                        lb.len -= 1;
                    }
                    echo(b"\x08 \x08");
                }
            }
            ch if ch >= 0x20 || (ch & 0xC0) == 0x80 => {
                // Printable ASCII or UTF-8 continuation byte
                if lb.len < LINE_BUF_CAP - 1 {
                    lb.buf[lb.len] = ch;
                    lb.len += 1;
                    echo(&[ch]);
                }
            }
            ch if (ch & 0xC0) == 0xC0 => {
                // UTF-8 lead byte
                if lb.len < LINE_BUF_CAP - 1 {
                    lb.buf[lb.len] = ch;
                    lb.len += 1;
                    echo(&[ch]);
                }
            }
            _ => {} // ignore other control characters
        }
    }
}

// ---------------------------------------------------------------------------
// Stdin
// ---------------------------------------------------------------------------

pub struct Stdin;
pub struct Stdout;
pub struct Stderr;

impl Stdin {
    pub const fn new() -> Stdin {
        Stdin
    }
}

impl io::Read for Stdin {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let stat = syscall::fstat(STDIN).ok();
        let interactive =
            stat.is_some_and(|s| s.file_type == FileType::Keyboard || s.file_type == FileType::Tty);
        if STDIN_RAW.load(Ordering::Relaxed) || !interactive {
            syscall::read(STDIN, buf).map_err(to_io_error)
        } else {
            canonical_read(buf)
        }
    }

    fn read_vectored(&mut self, bufs: &mut [IoSliceMut<'_>]) -> io::Result<usize> {
        let buf = match bufs.first_mut() {
            Some(b) => b,
            None => return Ok(0),
        };
        self.read(buf)
    }

    fn is_read_vectored(&self) -> bool {
        false
    }
}

impl Stdout {
    pub const fn new() -> Stdout {
        Stdout
    }
}

impl io::Write for Stdout {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        log_stdio::write(Stream::Out, buf).map_err(to_io_error)
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        let mut total = 0;
        for buf in bufs {
            total += self.write(buf)?;
        }
        Ok(total)
    }

    fn is_write_vectored(&self) -> bool {
        false
    }

    fn flush(&mut self) -> io::Result<()> {
        log_stdio::flush(Stream::Out);
        Ok(())
    }
}

impl Stderr {
    pub const fn new() -> Stderr {
        Stderr
    }
}

impl io::Write for Stderr {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        log_stdio::write(Stream::Err, buf).map_err(to_io_error)
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        let mut total = 0;
        for buf in bufs {
            total += self.write(buf)?;
        }
        Ok(total)
    }

    fn is_write_vectored(&self) -> bool {
        false
    }

    fn flush(&mut self) -> io::Result<()> {
        log_stdio::flush(Stream::Err);
        Ok(())
    }
}

pub const STDIN_BUF_SIZE: usize = 64;

pub fn is_ebadf(_err: &io::Error) -> bool {
    true
}

pub fn panic_output() -> Option<Stderr> {
    Some(Stderr::new())
}

/// What either stream holds of a line the process has not ended, written out
/// as that line's end as the process leaves: a stream that is a log ring
/// keeps a partial line until its newline.
pub fn finish() {
    log_stdio::end(Stream::Out);
    log_stdio::end(Stream::Err);
}
