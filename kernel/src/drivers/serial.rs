//! The 16550 and the virtio-console, and the two locks that serialise them.
//!
//! **One writer puts lines on the wire: whoever holds [`WIRE`]**, which is
//! `klogd` — the kernel's records and the lines console holders queue for it —
//! and the few drains that stand in for it. It is held for a whole line, so
//! a line is one writer's, and with interrupts on. [`BackendGuard`] is the
//! registers' lock, held with interrupts off for one burst: a FIFO's worth
//! to a 16550, a transmit buffer's to virtio-console, a byte read. The panic
//! path takes the registers alone, and bypasses them once they stay held.
//! Nothing that holds a kernel lock formats here.

use core::sync::atomic::{AtomicBool, Ordering};
use crate::arch::cpu::{inb, outb};
use crate::log;
use crate::scheduler::Parkable;
use crate::sleeplock::{SleepGuard, SleepLock};

const PORT: u16 = 0x3f8; // COM1

// Latched once from `init`'s loopback probe: hardware with no SuperIO
// reads 0xFF on every access, indistinguishable from a ready UART.
static UART_PRESENT: AtomicBool = AtomicBool::new(false);

// Every register is `PORT + n`; the identity op keeps that pattern uniform
// across all eight lines instead of special-casing the data register.
#[allow(clippy::identity_op)]
pub fn init() {
    // SAFETY: `outb`/`inb` require the caller to own the port and the byte;
    // every port here is `PORT + n` for `n` in 0..=4, inside COM1's own
    // register block, and the writes are the 16550's documented init sequence.
    // Order matters: DLAB must precede the divisor writes and loopback mode
    // must precede the probe, or the sequence misprograms the chip.
    let loopback = unsafe {
        outb(PORT + 1, 0x00); // Disable all interrupts
        outb(PORT + 3, 0x80); // Enable DLAB (set baud rate divisor)
        outb(PORT + 0, 0x03); // Set divisor to 3 (lo byte) 38400 baud
        outb(PORT + 1, 0x00); //                  (hi byte)
        outb(PORT + 3, 0x03); // 8 bits, no parity, one stop bit
        outb(PORT + 2, 0xC7); // Enable FIFO, clear them, with 14-byte threshold
        outb(PORT + 4, 0x0B); // IRQs enabled, RTS/DSR set
        outb(PORT + 4, 0x1E); // Set in loopback mode, test the serial chip
        outb(PORT + 0, 0xAE); // Test serial chip (send byte 0xAE and check if serial returns same byte)
        let seen = inb(PORT + 0);
        UART_PRESENT.store(seen == 0xAE, Ordering::Relaxed);
        outb(PORT + 4, 0x0F); // Normal operation mode
        seen
    };
    // Logs the raw byte, not just the verdict: distinguishes "no SuperIO"
    // (0xFF) from a wrong response and a right chip at the wrong port.
    log!(
        "serial: 16550 loopback read {:#04x} ({})",
        loopback,
        if loopback == 0xAE { "present" } else { "absent or wrong port" }
    );
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

/// Which channel a write goes to right now; virtio-console is preferred over a 16550.
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
    rflags: SavedFlags,
}

/// This CPU's own `RFLAGS`, captured by `pushfq`; the only value `popfq` may be given.
/// Not `Copy`/`Clone`: one CPU's state at one instant, not to be duplicated.
pub struct SavedFlags(u64);

impl SavedFlags {
    /// Restores the flags; `&self` because `Drop` cannot move a field out, and restoring twice is idempotent.
    #[inline]
    fn restore(&self) {
        // SAFETY: `popfq` has no safe spelling; `self.0` came only from this
        // CPU's own `pushfq` in `save_and_cli`, so no unintended bit reaches RFLAGS.
        unsafe {
            core::arch::asm!(
                "push {}",
                "popfq",
                in(reg) self.0,
                options(nomem),
            );
        }
    }
}

impl BackendGuard {
    pub fn lock() -> Self {
        let rflags = save_and_cli();
        while BACKEND_LOCKED
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            while BACKEND_LOCKED.load(Ordering::Relaxed) {
                core::hint::spin_loop();
            }
        }
        Self { rflags }
    }

    /// Non-blocking acquire: `None` if another CPU already holds the backend.
    pub fn try_lock() -> Option<Self> {
        let rflags = save_and_cli();
        if BACKEND_LOCKED
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            Some(Self { rflags })
        } else {
            rflags.restore();
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
            uart_present() && inb(PORT + 5) & 0x01 != 0
        }
    }

    pub fn try_read_byte(&mut self) -> Option<u8> {
        if super::virtio_console::is_ready() {
            super::virtio_console::try_read_byte_locked()
        } else if uart_present() && inb(PORT + 5) & 0x01 != 0 {
            Some(inb(PORT))
        } else {
            None
        }
    }
}

impl Drop for BackendGuard {
    fn drop(&mut self) {
        BACKEND_LOCKED.store(false, Ordering::Release);
        self.rflags.restore();
    }
}

/// This CPU's `RFLAGS`, captured with interrupts off in one instruction sequence:
/// the value is stale if anything runs between the read and `cli`.
#[inline]
fn save_and_cli() -> SavedFlags {
    let rflags: u64;
    // SAFETY: irreducible — `pushfq`/`cli` have no safe spelling; the asm reads
    // RFLAGS and clears IF only, writes no memory, and touches no other register.
    unsafe {
        core::arch::asm!(
            "pushfq",
            "pop {}",
            "cli",
            out(reg) rflags,
            options(nomem),
        );
    }
    SavedFlags(rflags)
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
/// Bounded on the wire like `panic_flush`, but never bypasses: every CPU is
/// still live here, and reading the ring unsynchronized is only safe once
/// nothing else runs. Losing the tail is better than not powering off.
pub fn flush_final() {
    for _ in 0..PANIC_LOCK_SPIN_LIMIT {
        if let Some(wire) = try_wire() {
            crate::log::console::drain_all(&wire);
            return;
        }
        core::hint::spin_loop();
    }
}

/// Who may put a line on the wire: `klogd`, and the few drains that stand in
/// for it (boot before it runs, the stop, the power-off). **Held with
/// interrupts on and preemption allowed**, for a whole line, so a line is one
/// holder's; [`BackendGuard`] is taken inside it once per burst, which is the
/// only interrupts-off window the console costs.
static WIRE: SleepLock<()> = SleepLock::new(());

/// The wire, for a task that may park until it is free.
pub fn wire(parkable: &Parkable) -> SleepGuard<'_, ()> {
    WIRE.lock(parkable)
}

/// The wire, if it is free, from any context — the boot before per-CPU state
/// exists included, which has no task to hold it as.
pub fn try_wire() -> Option<SleepGuard<'static, ()>> {
    if crate::log::PERCPU_READY.load(Ordering::Acquire) {
        WIRE.try_lock()
    } else {
        WIRE.try_lock_untasked()
    }
}

/// A 16550's transmit FIFO: once `LSR.THRE` reads set in FIFO mode the FIFO is
/// empty, and this many bytes may go in before it is asked again (PC16550D
/// data sheet, FIFO mode; `init` enables the FIFO).
const UART_FIFO: usize = 16;

/// `bytes` onto the wire the caller holds, in bursts: one FIFO's worth to a
/// 16550, one transmit buffer's to virtio-console, each under its own
/// [`BackendGuard`] and interrupts on between two.
pub fn write_wire(_wire: &SleepGuard<'_, ()>, bytes: &[u8]) {
    match backend() {
        Backend::Virtio => {
            for chunk in bytes.chunks(super::virtio_console::TX_BUF_SIZE) {
                let _burst = BackendGuard::lock();
                super::virtio_console::write_bytes_locked(chunk);
            }
        }
        Backend::Uart => uart_write_fifo(bytes),
        Backend::None => {}
    }
}

/// The FIFO burst writer: each burst waits for `THRE` with interrupts on, and
/// takes the register lock only to test the flag and fill the FIFO.
fn uart_write_fifo(bytes: &[u8]) {
    for chunk in bytes.chunks(UART_FIFO) {
        let mut asked = 0;
        loop {
            let burst = BackendGuard::lock();
            if inb(PORT + 5) & 0x20 != 0 {
                for &b in chunk {
                    // SAFETY: COM1's own data register; the FIFO is empty and
                    // takes `UART_FIFO` bytes, and a chunk is no more.
                    unsafe { outb(PORT, b) };
                }
                break;
            }
            drop(burst);
            asked += 1;
            // A UART that never empties its FIFO takes the rest of this write
            // with it rather than holding the wire for ever.
            if asked == THRE_SPIN_LIMIT {
                return;
            }
            core::hint::spin_loop();
        }
    }
}

/// A userland `write` to the console as it arrives, with no line kept: only
/// the `console-unbuffered` actuator's, which is the negative control on the
/// line buffer every console otherwise is. Whatever finds the queue full is
/// counted unshown.
pub fn write_console(src: &crate::user_ptr::UserBytes) -> usize {
    let mut chunk = [0u8; MAX_CONSOLE_LINE];
    let mut off = 0;
    while off < src.len() {
        let n = chunk.len().min(src.len() - off);
        src.read_at(off, &mut chunk[..n]);
        if !crate::log::console::queue(&chunk[..n]) {
            crate::log::console::unshown();
        }
        off += n;
    }
    src.len()
}

/// One console holder's partly-written line. Must live per holder, never
/// shared: one buffer used by two processes splices their output.
///
/// **A console holder does not write the wire.** A whole line goes to
/// `klogd`'s queue ([`crate::log::console::queue`]), and `klogd` puts it on
/// the wire between its own records — so the kernel has one console writer,
/// and a write here never waits on a device. The bytes go as they came: the
/// only holder that writes is `/system/bin/logd`, which renders every control
/// byte a program wrote as text before it gets here.
///
/// **A write takes the whole lines the queue has room for and no more**, and
/// says how many bytes that was, so a writer that is ahead of the console is
/// told so rather than losing lines it cannot see; its poll for `WRITABLE` is
/// answered when `klogd` frees room.
pub struct ConsoleLine {
    buf: [u8; MAX_CONSOLE_LINE],
    len: usize,
    /// `buf` holds a whole line the queue had no room for, which goes first.
    held: bool,
}

impl ConsoleLine {
    pub const fn new() -> Self {
        Self { buf: [0; MAX_CONSOLE_LINE], len: 0, held: false }
    }

    /// Take as much of a userland write as ends in lines the queue has room
    /// for, and a trailing partial line; answer how many bytes were taken.
    pub fn write(&mut self, src: &crate::user_ptr::UserBytes) -> usize {
        if self.held && !self.release() {
            return 0;
        }
        let mut chunk = [0u8; STRIP_CHUNK];
        let mut off = 0;
        while off < src.len() {
            let n = chunk.len().min(src.len() - off);
            src.read_at(off, &mut chunk[..n]);
            for (i, &b) in chunk[..n].iter().enumerate() {
                if self.len == MAX_CONSOLE_LINE && !self.close() {
                    return off + i;
                }
                self.buf[self.len] = b;
                self.len += 1;
                if b == b'\n' && !self.close() {
                    // Taken: the line is this holder's to send, and it goes
                    // ahead of the next write.
                    return off + i + 1;
                }
            }
            off += n;
        }
        src.len()
    }

    /// Queue the line `buf` holds; `false` keeps it held for the next write.
    fn close(&mut self) -> bool {
        self.held = true;
        self.release()
    }

    fn release(&mut self) -> bool {
        if !crate::log::console::queue(&self.buf[..self.len]) {
            return false;
        }
        self.len = 0;
        self.held = false;
        true
    }

    /// Queues whatever is held, whether or not a newline came, as the holder
    /// goes. With the queue full it is counted unshown: nothing waits here.
    pub fn finish(&mut self) {
        if self.len > 0 && !crate::log::console::queue(&self.buf[..self.len]) {
            crate::log::console::unshown();
        }
        self.len = 0;
        self.held = false;
    }
}

impl Default for ConsoleLine {
    fn default() -> Self {
        Self::new()
    }
}

/// Size of one copy out of user memory.
const STRIP_CHUNK: usize = 256;

/// The longest piece of a line one queue entry carries.
pub const MAX_CONSOLE_LINE: usize = 1024;

/// Bounded, not belt-and-braces: a UART wedged with THRE clear would spin
/// forever here, on `panic_flush`'s bypass path where nothing else can help.
const THRE_SPIN_LIMIT: u32 = 100_000;

fn uart_write_bytes(bytes: &[u8]) {
    if !uart_present() {
        return;
    }
    for &b in bytes {
        for _ in 0..THRE_SPIN_LIMIT {
            if inb(PORT + 5) & 0x20 != 0 {
                break;
            }
            core::hint::spin_loop();
        }
        // SAFETY: `outb` requires ownership of the port and the byte; `PORT`
        // is COM1's own data register, and the byte is console output only.
        unsafe { outb(PORT, b) };
    }
}

/// Writes straight to the 16550, bypassing the ring, the lock and virtio-console: no allocation, bounded per byte.
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
