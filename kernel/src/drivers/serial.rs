//! The console UART, as the kernel writes it (`log::console`), and the lock
//! that serialises the kernel's writers. Where the UART is and what it is are
//! the architecture's (`arch::console_uart`).
//!
//! [`BackendGuard`] is the registers' lock, held with interrupts off for one
//! burst: what the UART's transmitter takes at once. The panic path takes the
//! registers alone, as a [`PanicUart`], and bypasses them once they stay held.
//! Nothing that holds a kernel lock formats here.

use core::cell::Cell;
use core::sync::atomic::{AtomicBool, Ordering};
use crate::arch::IrqGuard;
use crate::log;
use super::serial_lock::{BackendLock, Held, Seized};
use crate::time::{Budget, Duration};

use crate::arch::console_uart as uart;

// Latched once from `init`: the architecture's own answer about whether a UART
// is there, since one that is not may still read as ready.
static UART_PRESENT: AtomicBool = AtomicBool::new(false);

/// Find and program the console UART, off the firmware tables at `rsdp_addr`
/// where the architecture places it by them, and put the boot so far on it.
pub fn init(rsdp_addr: u64) {
    // What the architecture logs under the lock is not drained under it:
    // `has_console` is false until the store below.
    let present = uart::init(&mut BackendGuard::lock().registers, rsdp_addr);
    UART_PRESENT.store(present, Ordering::Relaxed);
    crate::log::console::drain_inline();
}

/// Whether anything can carry a byte off this machine; the same check `panic_flush` refuses on.
pub fn has_console() -> bool {
    UART_PRESENT.load(Ordering::Relaxed)
}

static BACKEND: BackendLock = BackendLock::new();

/// The console UART's registers, asked for: what every `arch::console_uart`
/// function that touches the UART takes. Only this file builds one, for a
/// [`BackendGuard`] and a [`PanicUart`].
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
        uart_write_bytes(&mut self.registers, bytes);
    }
}

/// How long, on the clock, a fatal path waits for another holder to let the
/// console registers go: far past a burst's hold, and short of holding up a
/// report behind a holder that will never let go.
const PANIC_LOCK_WAIT: Budget = Budget::of(
    Duration::from_secs(1),
    "the fatal path says so and writes over the holder",
);

/// The console UART's registers for a fatal path, from [`panic_registers`].
/// A `log!` under one before the hand-over may drain inline on this CPU, as a
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
    let seized = Cell::new(None);
    // Uncalibrated, this asks once: only the boot CPU runs, so a holder is beneath this frame and never lets go.
    crate::clock::settles(PANIC_LOCK_WAIT.nanos(), || {
        let answer = BACKEND.try_seize();
        let answered = answer.is_some();
        seized.set(answer);
        answered
    });
    PanicUart(match seized.into_inner() {
        Some(Seized::Taken(held)) => Hold::Held(BackendGuard { _held: held, _irq: irq, registers: Registers(()) }),
        Some(Seized::Reentered) => Hold::Reentered(Over { registers: Registers(()), _irq: irq }),
        None => {
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

    /// Straight to the UART: no allocation, bounded per byte.
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
/// Waits for a live guard holder to release before bypassing it, and only
/// bypasses a holder that never releases, or its own CPU's fatal path.
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
    // SAFETY: the registers are a wedged holder's or a fatal path's beneath
    // this one, neither of which runs again to publish, so reading the
    // position unlocked is safe.
    unsafe { crate::log::console::drain_bypassed(&mut uart) };
}

/// Whether the UART has refused a write, which is said once.
static UART_REFUSED: AtomicBool = AtomicBool::new(false);

/// `bytes` onto the UART, in bursts of what its transmitter takes at once:
/// each burst waits for the transmitter with interrupts on, and takes the
/// register lock only to ask it and fill it. The boot's and the stop's,
/// whose drains are the UART's one writer while they write.
pub fn write(bytes: &[u8]) {
    if !has_console() {
        return;
    }
    for chunk in bytes.chunks(uart::TX_BURST) {
        let mut asked = 0;
        loop {
            let mut burst = BackendGuard::lock();
            if uart::tx_ready(&mut burst.registers) {
                // A chunk is no more than the transmitter takes once ready.
                for &b in chunk {
                    uart::write_byte(&mut burst.registers, b);
                }
                break;
            }
            drop(burst);
            asked += 1;
            // A UART that never empties its FIFO takes the rest of this write
            // with it rather than holding the boot for ever, and is said once:
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

/// Bounded, not belt-and-braces: a UART wedged with THRE clear would spin
/// forever here, on `panic_flush`'s bypass path where nothing else can help.
const THRE_SPIN_LIMIT: u32 = 100_000;

fn uart_write_bytes(registers: &mut Registers, bytes: &[u8]) {
    if !has_console() {
        return;
    }
    for &b in bytes {
        for _ in 0..THRE_SPIN_LIMIT {
            if uart::tx_ready(registers) {
                break;
            }
            core::hint::spin_loop();
        }
        uart::write_byte(registers, b);
    }
}
