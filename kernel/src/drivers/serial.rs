//! The console UART and the virtio-console, and the one lock that serialises
//! them. Where the UART is and what it is are the architecture's
//! (`arch::console_uart`).
//! Every writer takes [`BackendGuard`] once per whole unit (a record, a
//! userland `write`, a panic report) and holds it for that whole unit; that
//! is the only source of line atomicity. Every unit taken under the guard is
//! bounded, except the panic path's `drain_locked`. Nothing that holds a
//! kernel lock formats here.

use core::sync::atomic::{AtomicBool, Ordering};
use crate::arch::IrqGuard;

use crate::arch::console_uart as uart;

// Latched once from `init`: the architecture's own answer about whether a UART
// is there, since one that is not may still read as ready.
static UART_PRESENT: AtomicBool = AtomicBool::new(false);

/// Find and program the console UART, off the firmware tables at `rsdp_addr`
/// where the architecture places it by them.
pub fn init(rsdp_addr: u64) {
    UART_PRESENT.store(uart::init(rsdp_addr), Ordering::Relaxed);
    console_changed();
}

/// A backend arrived or improved. Forwards to `log::console`, which owns the replay argument.
pub fn console_changed() {
    crate::log::console::backend_changed();
}

pub fn uart_present() -> bool {
    UART_PRESENT.load(Ordering::Relaxed)
}

/// Whether anything can carry a byte off this machine; the same check `panic_flush` refuses on.
pub fn has_console() -> bool {
    !matches!(backend(), Backend::None)
}

/// Which channel a write goes to right now; virtio-console is preferred over the UART.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Backend {
    /// Nothing can carry a byte off this machine.
    None = 0,
    Uart = 1,
    Virtio = 2,
}

pub fn backend() -> Backend {
    if super::virtio_console::is_ready() {
        Backend::Virtio
    } else if uart_present() {
        Backend::Uart
    } else {
        Backend::None
    }
}


static BACKEND_LOCKED: AtomicBool = AtomicBool::new(false);

/// Exclusive access to the serial backend; interrupts are off for as long as the guard lives.
/// Same-CPU re-entry from an IRQ handler deadlocks the spin.
pub struct BackendGuard {
    // Dropped after `Drop::drop` releases the backend, so interrupts reopen last.
    _irq: IrqGuard,
}

impl BackendGuard {
    pub fn lock() -> Self {
        let irq = IrqGuard::close();
        while BACKEND_LOCKED
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            while BACKEND_LOCKED.load(Ordering::Relaxed) {
                core::hint::spin_loop();
            }
        }
        Self { _irq: irq }
    }

    /// Non-blocking acquire: `None` if another CPU already holds the backend.
    pub fn try_lock() -> Option<Self> {
        let irq = IrqGuard::close();
        // Not `then_some`: its argument is built whether or not the exchange
        // won, and a `BackendGuard` built on a loss drops, and its drop
        // releases the backend another CPU holds.
        if BACKEND_LOCKED
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            Some(Self { _irq: irq })
        } else {
            None
        }
    }

    /// Writes raw bytes with no escape stripping; callers must pre-strip via [`write_console`].
    pub fn write_raw(&mut self, bytes: &[u8]) {
        match backend() {
            Backend::Virtio => super::virtio_console::write_bytes_locked(bytes),
            Backend::Uart => uart_write_bytes(bytes),
            Backend::None => {}
        }
    }

    pub fn has_data(&self) -> bool {
        if super::virtio_console::is_ready() {
            super::virtio_console::has_data_locked()
        } else {
            uart_present() && uart::rx_ready()
        }
    }

    pub fn try_read_byte(&mut self) -> Option<u8> {
        if super::virtio_console::is_ready() {
            super::virtio_console::try_read_byte_locked()
        } else if uart_present() && uart::rx_ready() {
            Some(uart::read_byte())
        } else {
            None
        }
    }
}

impl Drop for BackendGuard {
    fn drop(&mut self) {
        BACKEND_LOCKED.store(false, Ordering::Release);
    }
}

pub fn has_data() -> bool {
    let g = BackendGuard::lock();
    g.has_data()
}

pub fn try_read_byte() -> Option<u8> {
    let mut g = BackendGuard::lock();
    g.try_read_byte()
}

/// ~1s of spin, long enough for a live guard holder to release and short enough not to hang panic.
const PANIC_LOCK_SPIN_LIMIT: u64 = 100_000_000;

/// Flushes pending logs on the panic path.
///
/// Waits for a live guard holder to release before bypassing it — bypassing
/// immediately would race its live ring/virtqueue mutation — and only
/// bypasses a holder that never releases.
///
/// # Safety
/// Panic context only: the bypass reads the drain position with no lock held.
pub unsafe fn panic_flush() {
    // Checked before the locked path: with no backend, that path would just
    // discard the report while still advancing the drain past it.
    if !has_console() {
        return;
    }
    for _ in 0..PANIC_LOCK_SPIN_LIMIT {
        if let Some(mut g) = BackendGuard::try_lock() {
            crate::log::console::drain_locked(&mut g);
            return;
        }
        core::hint::spin_loop();
    }
    // Disables virtio-console first: a half-submitted TX queue would panic
    // recursively if a bypassing write reached it.
    if !uart_present() {
        return;
    }
    super::virtio_console::disable();
    // SAFETY: the bounded wait above found no clean handoff; the holder is
    // wedged and will not publish, so reading its position unlocked is safe.
    unsafe { crate::log::console::drain_bypassed() };
}

/// Drains the ring before the machine powers off, so the tail of a shutdown
/// is not lost to `acpi::shutdown()` cutting power with logs still queued.
///
/// Bounded on the lock like `panic_flush`, but never bypasses: every CPU is
/// still live here, and reading the ring unsynchronized is only safe once
/// nothing else runs. Losing the tail is better than not powering off.
pub fn flush_final() {
    for _ in 0..PANIC_LOCK_SPIN_LIMIT {
        if let Some(mut g) = BackendGuard::try_lock() {
            crate::log::console::drain_locked(&mut g);
            return;
        }
        core::hint::spin_loop();
    }
}

/// A userland `write` to the console, unbuffered and ANSI-stripped.
pub fn write_console(src: &crate::user_ptr::UserBytes) {
    let mut line = ConsoleLine::new();
    line.out.on_newline = false;
    line.write(src);
    // Nothing held back: a trailing ESC is the caller's own byte, emitted here too.
    line.finish();
}

/// One console holder's partly-written line. Must live per holder, never
/// shared: one buffer used by two processes splices their output.
pub struct ConsoleLine {
    out: Stripped,
    csi: Csi,
}

impl ConsoleLine {
    pub const fn new() -> Self {
        Self {
            out: Stripped { buf: [0; MAX_CONSOLE_LINE], len: 0, on_newline: true },
            csi: Csi::Text,
        }
    }

    /// Accumulate a userland write, emitting every whole line it completes.
    pub fn write(&mut self, src: &crate::user_ptr::UserBytes) {
        let mut chunk = [0u8; STRIP_CHUNK];
        let mut off = 0;
        while off < src.len() {
            let n = chunk.len().min(src.len() - off);
            src.read_at(off, &mut chunk[..n]);
            self.csi.feed(&mut self.out, &chunk[..n]);
            off += n;
        }
    }

    /// Emits whatever is held back, whether or not a newline came; dropping
    /// it here would lose output the process already wrote.
    pub fn finish(&mut self) {
        let csi = core::mem::replace(&mut self.csi, Csi::Text);
        csi.finish(&mut self.out);
        self.out.flush();
    }
}

impl Default for ConsoleLine {
    fn default() -> Self {
        Self::new()
    }
}

/// Size of one copy out of user memory; not the backend's unit — see [`MAX_CONSOLE_LINE`].
const STRIP_CHUNK: usize = 256;

/// The most written to the backend under one [`BackendGuard`], bounding a write's interrupts-off window.
const MAX_CONSOLE_LINE: usize = 1024;

/// Buffers bytes for the backend: a per-byte filter must not become a per-byte device write or lock acquisition.
/// Holds no guard of its own; interrupts are on between two chunks of one write.
struct Stripped {
    buf: [u8; MAX_CONSOLE_LINE],
    len: usize,
    /// Whether a newline ends a unit; true for a line buffer, false for [`write_console`]'s unbuffered chunking.
    on_newline: bool,
}

impl Stripped {
    fn push_byte(&mut self, b: u8) {
        if self.len == MAX_CONSOLE_LINE {
            self.flush();
        }
        self.buf[self.len] = b;
        self.len += 1;
        if self.on_newline && b == b'\n' {
            self.flush();
        }
    }

    fn flush(&mut self) {
        if self.len > 0 {
            BackendGuard::lock().write_raw(&self.buf[..self.len]);
            self.len = 0;
        }
    }
}

/// Strips ANSI CSI sequences; a state machine because writes and flushes arrive in different-sized chunks.
enum Csi {
    Text,
    /// An ESC held back: only the start of a sequence if `[` follows.
    Esc,
    Body,
}

impl Csi {
    fn feed(&mut self, out: &mut Stripped, bytes: &[u8]) {
        for &b in bytes {
            match self {
                Self::Text if b == 0x1B => *self = Self::Esc,
                Self::Text => out.push_byte(b),
                Self::Esc if b == b'[' => *self = Self::Body,
                Self::Esc => {
                    out.push_byte(0x1B);
                    *self = Self::Text;
                    if b == 0x1B { *self = Self::Esc } else { out.push_byte(b) }
                }
                Self::Body if (0x40..=0x7E).contains(&b) => *self = Self::Text,
                Self::Body => {}
            }
        }
    }

    /// A sequence the input ended mid-way: a lone ESC is emitted, a started CSI body is not.
    fn finish(self, out: &mut Stripped) {
        if matches!(self, Self::Esc) {
            out.push_byte(0x1B);
        }
    }
}

/// Bounded, not belt-and-braces: a UART wedged with THRE clear would spin
/// forever here, on `panic_flush`'s bypass path where nothing else can help.
const THRE_SPIN_LIMIT: u32 = 100_000;

fn uart_write_bytes(bytes: &[u8]) {
    if !uart_present() {
        return;
    }
    for &b in bytes {
        for _ in 0..THRE_SPIN_LIMIT {
            if uart::tx_ready() {
                break;
            }
            core::hint::spin_loop();
        }
        uart::write_byte(b);
    }
}

/// Writes straight to the UART, bypassing the ring, the lock and virtio-console: no allocation, bounded per byte.
pub fn panic_raw(bytes: &[u8]) {
    uart_write_bytes(bytes);
}

/// `panic_raw` for an address, formatted as `{:#018x}` to match the rest of the crash report.
pub fn panic_raw_hex(v: u64) {
    let mut out = [b'0'; 18];
    out[1] = b'x';
    for (i, byte) in out[2..].iter_mut().enumerate() {
        let nibble = (v >> (60 - 4 * i)) as u8 & 0xF;
        *byte = if nibble < 10 { b'0' + nibble } else { b'a' + nibble - 10 };
    }
    uart_write_bytes(&out);
}

/// `panic_raw` for a number, since the callers cannot format one.
pub fn panic_raw_dec(mut v: u64) {
    let mut digits = [0u8; 20];
    let mut n = 0;
    loop {
        digits[n] = b'0' + (v % 10) as u8;
        n += 1;
        v /= 10;
        if v == 0 || n == digits.len() {
            break;
        }
    }
    let mut out = [0u8; 20];
    for i in 0..n {
        out[i] = digits[n - 1 - i];
    }
    uart_write_bytes(&out[..n]);
}
