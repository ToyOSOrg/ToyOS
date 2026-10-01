//! The console UART and the virtio-console, and the two locks that serialise
//! them. Where the UART is and what it is are the architecture's
//! (`arch::console_uart`).
//!
//! [`BackendGuard`] is the registers' lock, held with interrupts off for one
//! burst: what the UART's transmitter takes at once; the publish of a
//! transmit buffer to virtio-console, and each look for its completion, which
//! the host takes at its own pace; a byte read. The panic path takes the
//! registers alone, as a [`PanicUart`], and bypasses them once they stay held.
//! Nothing that holds a kernel lock formats here.

use core::sync::atomic::{AtomicBool, Ordering};
use crate::arch::IrqGuard;
use crate::log;
use super::serial_lock::{self, BackendLock, Held, Seized};
use crate::scheduler::Parkable;
use crate::sleeplock::{SleepGuard, SleepLock};

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


static BACKEND: BackendLock = BackendLock::new();

/// The console UART's registers, asked for: what `arch::console_uart` moves a
/// byte with. Only this file builds one, for a [`BackendGuard`] and a
/// [`PanicUart`].
pub struct Registers(());

/// Exclusive access to the serial backend; interrupts are off for as long as the guard lives.
/// Same-CPU re-entry from an IRQ handler deadlocks the spin.
pub struct BackendGuard {
    // Fields drop in order: the backend is released before interrupts reopen.
    _held: Held<'static>,
    _irq: IrqGuard,
    registers: Registers,
}

impl BackendGuard {
    pub fn lock() -> Self {
        let irq = IrqGuard::close();
        Self { _held: BACKEND.lock(), _irq: irq, registers: Registers(()) }
    }

    /// Writes raw bytes with no escape stripping.
    pub fn write_raw(&mut self, bytes: &[u8]) {
        match backend() {
            Backend::Virtio => super::virtio_console::write_bytes_locked(self, bytes),
            Backend::Uart => uart_write_bytes(&mut self.registers, bytes),
            Backend::None => {}
        }
    }

    pub fn has_data(&self) -> bool {
        if super::virtio_console::is_ready() {
            super::virtio_console::has_data_locked(self)
        } else {
            uart_present() && uart::rx_ready()
        }
    }

    pub fn try_read_byte(&mut self) -> Option<u8> {
        if super::virtio_console::is_ready() {
            super::virtio_console::try_read_byte_locked(self)
        } else if uart_present() && uart::rx_ready() {
            Some(uart::read_byte(&mut self.registers))
        } else {
            None
        }
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

/// The console UART's registers for a fatal path, from [`panic_registers`].
/// A `log!` under one before `klogd` runs may drain inline on this CPU, as a
/// burst that `BackendLock::lock` refuses while this CPU's fatal path holds
/// the registers.
pub struct PanicUart(Hold);

enum Hold {
    /// Taken from nobody, or from a holder that let go inside the bound.
    Held(BackendGuard),
    /// This CPU's own fatal path holds the registers, underneath this one.
    Reentered(Over),
    /// Another holder kept them through the whole bound, which was said.
    Expired(Over),
}

/// The registers, written over whoever holds them.
struct Over {
    registers: Registers,
    _irq: IrqGuard,
}

/// What a fatal path that waited out another holder writes first.
const WRITTEN_OVER: &[u8] =
    b"\n[serial] the console registers stayed held through the bound; written over their holder\n";

/// What a flush that found its own CPU's fatal path holding the registers writes first.
const DRAINED_RAW: &[u8] =
    b"\n[serial] this cpu's own fatal path held the console registers; drained raw\n";

/// The registers for a fatal path: waited for while another holder may still
/// let them go, and had at once where this CPU's fatal path holds them.
pub fn panic_registers() -> PanicUart {
    let irq = IrqGuard::close();
    PanicUart(match BACKEND.seize(PANIC_LOCK_SPIN_LIMIT) {
        Seized::Taken(held) => Hold::Held(BackendGuard { _held: held, _irq: irq, registers: Registers(()) }),
        Seized::Reentered => Hold::Reentered(Over { registers: Registers(()), _irq: irq }),
        Seized::Expired => {
            let mut over = Over { registers: Registers(()), _irq: irq };
            uart_write_bytes(&mut over.registers, WRITTEN_OVER);
            Hold::Expired(over)
        }
    })
}

impl PanicUart {
    fn registers(&mut self) -> &mut Registers {
        match &mut self.0 {
            Hold::Held(guard) => &mut guard.registers,
            Hold::Reentered(over) | Hold::Expired(over) => &mut over.registers,
        }
    }

    /// Straight to the UART, never virtio-console: no allocation, bounded per byte.
    pub fn write(&mut self, bytes: &[u8]) {
        uart_write_bytes(self.registers(), bytes);
    }

    /// An address, formatted as `{:#018x}` to match the rest of the crash report.
    pub fn hex(&mut self, v: u64) {
        let mut out = [b'0'; 18];
        out[1] = b'x';
        for (i, byte) in out[2..].iter_mut().enumerate() {
            let nibble = (v >> (60 - 4 * i)) as u8 & 0xF;
            *byte = if nibble < 10 { b'0' + nibble } else { b'a' + nibble - 10 };
        }
        uart_write_bytes(self.registers(), &out);
    }

    /// A number, since the callers cannot format one.
    pub fn dec(&mut self, mut v: u64) {
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
        uart_write_bytes(self.registers(), &out[..n]);
    }
}

/// Flushes pending logs on the panic path.
///
/// Waits for a live guard holder to release before bypassing it — bypassing
/// immediately would race its live ring/virtqueue mutation — and only
/// bypasses a holder that never releases, or its own CPU's fatal path, which
/// may have stopped inside a virtqueue publish.
///
/// # Safety
/// Panic context only: the bypass reads the drain position with no lock held.
pub unsafe fn panic_flush() {
    // Checked before the locked path: with no backend, that path would just
    // discard the report while still advancing the drain past it.
    if !has_console() {
        return;
    }
    let mut uart = panic_registers();
    if let Hold::Held(guard) = &mut uart.0 {
        crate::log::console::drain_locked(guard);
        return;
    }
    if matches!(uart.0, Hold::Reentered(_)) {
        uart.write(DRAINED_RAW);
    }
    // Disables virtio-console first: a half-submitted TX queue would panic
    // recursively if a bypassing write reached it.
    if !uart_present() {
        return;
    }
    super::virtio_console::disable();
    // SAFETY: the registers are a wedged holder's or a fatal path's beneath
    // this one, neither of which runs again to publish, so reading the
    // position unlocked is safe.
    unsafe { crate::log::console::drain_bypassed(&mut uart) };
}

/// Drains the ring before the machine powers off, so the tail of a shutdown
/// is not lost to `acpi::shutdown()` cutting power with logs still queued.
///
/// Bounded on the wire like `panic_flush`, but never bypasses: every CPU is
/// still live here, and reading the ring unsynchronized is only safe once
/// nothing else runs. Losing the tail is better than not powering off, and
/// the black box says it was lost, since the console cannot.
pub fn flush_final() {
    if let Some(wire) = serial_lock::within(PANIC_LOCK_SPIN_LIMIT, try_wire) {
        crate::log::console::drain_all(&wire);
        return;
    }
    crate::blackbox::append(|lines| {
        let _ = writeln!(
            lines,
            "console: the wire stayed held through the stop's last drain, so this boot's last \
             records are not on the console"
        );
    });
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

/// Whether the UART has refused a write, which is said once.
static UART_REFUSED: AtomicBool = AtomicBool::new(false);

/// `bytes` onto the wire the caller holds, in bursts: what the UART's
/// transmitter takes at once, one transmit buffer's to virtio-console, with interrupts on
/// between every two looks at the device.
pub fn write_wire(_wire: &SleepGuard<'_, ()>, bytes: &[u8]) {
    match backend() {
        Backend::Virtio => {
            for chunk in bytes.chunks(super::virtio_console::TX_BUF_SIZE) {
                super::virtio_console::write_burst(chunk);
            }
        }
        Backend::Uart => uart_write_fifo(bytes),
        Backend::None => {}
    }
}

/// The burst writer: each burst waits for the transmitter with interrupts on,
/// and takes the register lock only to ask it and fill it.
fn uart_write_fifo(bytes: &[u8]) {
    for chunk in bytes.chunks(uart::TX_BURST) {
        let mut asked = 0;
        loop {
            let mut burst = BackendGuard::lock();
            if uart::tx_ready() {
                // A chunk is no more than the transmitter takes once ready.
                for &b in chunk {
                    uart::write_byte(&mut burst.registers, b);
                }
                break;
            }
            drop(burst);
            asked += 1;
            // A UART that never empties its FIFO takes the rest of this write
            // with it rather than holding the wire for ever, and is said once:
            // the saying is itself a write it would drop.
            if asked == THRE_SPIN_LIMIT {
                if !UART_REFUSED.swap(true, Ordering::Relaxed) {
                    log!(
                        "console: the UART took no byte in {THRE_SPIN_LIMIT} looks; what it does \
                         not take is dropped from here on, and the log has it"
                    );
                }
                return;
            }
            core::hint::spin_loop();
        }
    }
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
    /// `buf` is a whole piece of a line the queue had no room for, which goes
    /// before anything after it; the holder still has the rest of the line.
    held: bool,
}

impl ConsoleLine {
    pub const fn new() -> Self {
        Self { buf: [0; MAX_CONSOLE_LINE], len: 0, held: false }
    }

    /// Take as much of a userland write as ends in lines the queue has room
    /// for, and a trailing partial line; answer how many bytes were taken.
    ///
    /// **A line that does not fit is not taken**: its bytes in this write go
    /// back to the holder, which writes them again once it has room. Taken and
    /// held here, the last line of a holder with nothing more to say would
    /// wait for a write that never comes.
    pub fn write(&mut self, src: &crate::user_ptr::UserBytes) -> usize {
        if self.held && !self.piece() {
            return 0;
        }
        let mut chunk = [0u8; STRIP_CHUNK];
        let mut off = 0;
        // Where this write's part of the line `buf` holds begins.
        let mut line = 0;
        while off < src.len() {
            let n = chunk.len().min(src.len() - off);
            src.read_at(off, &mut chunk[..n]);
            for (i, &b) in chunk[..n].iter().enumerate() {
                let at = off + i;
                if self.len == MAX_CONSOLE_LINE {
                    self.held = true;
                    if !self.piece() {
                        return at;
                    }
                    line = at;
                }
                self.buf[self.len] = b;
                self.len += 1;
                if b == b'\n' {
                    if !crate::log::console::queue(&self.buf[..self.len], false) {
                        self.len -= at + 1 - line;
                        return line;
                    }
                    self.len = 0;
                    line = at + 1;
                }
            }
            off += n;
        }
        src.len()
    }

    /// Queue the whole piece `buf` holds, which the next goes on from.
    fn piece(&mut self) -> bool {
        if !crate::log::console::queue(&self.buf[..self.len], true) {
            return false;
        }
        self.len = 0;
        self.held = false;
        true
    }

    /// Queues whatever is held, whether or not a newline came, as the holder
    /// goes. With the queue full it is counted unshown: nothing waits here.
    pub fn finish(&mut self) {
        if self.len > 0 && !crate::log::console::queue(&self.buf[..self.len], false) {
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

fn uart_write_bytes(registers: &mut Registers, bytes: &[u8]) {
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
        uart::write_byte(registers, b);
    }
}
