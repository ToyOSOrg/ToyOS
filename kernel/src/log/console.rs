//! The kernel's console wire, the UART, which the kernel writes only where no
//! process can: inline, as each record commits, until [`hand_over`] gives it
//! to its claim's holder before userland runs; from the moment the stop has
//! stopped userland to the machine's end ([`StopWire`]); and on a fatal path
//! ([`drain_locked`], [`drain_bypassed`]). Between the hand-over and either
//! end the UART is the holder's, which `logkeeper` feeds, and no record of
//! the kernel's touches it.
//!
//! **The console has spoken every record before [`DRAINED`]'s position in
//! its ring**: what the inline drain put on the UART, and from the hand-over
//! what `logkeeper` says the console's channels have delivered, by the
//! positions its own read reached ([`speak`]). The stop and a fatal path put
//! every record past them on the UART, so they repeat at most what a channel
//! delivered and had not said yet, and miss none of the kernel's.
//!
//! **One writer at a time**: the boot's, whose drains take turns through a
//! claim on the wire, which the hand-over takes for good; then the holder's;
//! then the stop's, once userland is stopped, or a fatal path's, once every
//! other CPU is.

use core::sync::atomic::{AtomicBool, Ordering};

use toyos_abi::syscall::SyscallError;

use toyos_abi::log::LogRecord;

use crate::drivers::serial::{self, BackendGuard, PanicUart};

use super::read::{drain_ordered, Cursor, Published, RecordSink, Shards};
use crate::time::{Duration, Tripwire};

/// Whether the UART is its claim's holder's; set once, at [`hand_over`].
static HANDED: AtomicBool = AtomicBool::new(false);

/// What the console has spoken, a position per ring: moved by the inline
/// drain, by [`speak`] for the console's channels, and by the stop and the
/// fatal paths, so a record never reaches the wire twice from the kernel.
static DRAINED: Published = Published::new();

/// A boot drain's claim on the wire, which a drain that finds it held leaves
/// to its holder: that drain walks to the newest commit before letting go.
/// The hand-over takes it and never gives it back.
static WIRE: AtomicBool = AtomicBool::new(false);

/// How long the hand-over waits for a boot drain on another CPU to end.
const DRAIN_HELD: Tripwire = Tripwire::absurd(
    Duration::from_secs(60),
    "a boot drain writes what the boot committed, and no boot commits a minute of the UART's rate",
);

/// Positions for each of the `cpus` CPUs' shards.
pub fn size(cpus: usize) {
    DRAINED.size(cpus);
}

/// Whether a record's producer drains it to the UART: before the hand-over.
pub fn inline() -> bool {
    !HANDED.load(Ordering::Acquire)
}

/// Put every committed record this boot has not yet spoken on the UART.
/// Declining loses nothing: the record stays committed, and the drain that
/// holds the wire takes it too.
pub fn drain_inline() {
    if !inline() || !serial::has_console() {
        return;
    }
    if WIRE.swap(true, Ordering::Acquire) {
        return;
    }
    drain_records();
    WIRE.store(false, Ordering::Release);
}

/// Give the UART to its claim's holder, having spoken every record committed
/// so far: the boot's last inline drain, under the wire's claim, which a
/// drain on another CPU finishes its walk with first, and which is never given
/// back. The boot CPU's, once, before userland can claim the UART.
pub fn hand_over() {
    let deadline = crate::clock::nanos_since_boot() + DRAIN_HELD.nanos();
    while WIRE.swap(true, Ordering::Acquire) {
        assert!(crate::clock::nanos_since_boot() < deadline, "log: a boot drain held the UART past {DRAIN_HELD}");
        core::hint::spin_loop();
    }
    if serial::has_console() {
        drain_records();
    }
    HANDED.store(true, Ordering::Release);
}

/// Every record the console has not spoken onto the UART, through its
/// registers a burst at a time.
fn drain_records() {
    drain_ordered(&Shards, &mut &DRAINED, &mut Spoken(serial::write));
}

/// The console's channels have delivered every record before the positions
/// `next`, one per ring, which `logkeeper`'s own read reached: what the
/// console has spoken moves to at least them, never back, and is answered.
/// A position past what its ring has issued is refused.
pub fn speak(next: &[u64]) -> Result<(usize, Cursor), SyscallError> {
    Cursor::from_reader(next, &Shards).ok_or(SyscallError::InvalidArgument)?;
    Ok((0, Cursor::at(DRAINED.advance(next))))
}

/// The console as the stop holds it, from the moment userland has stopped to
/// the machine's end. Never dropped: what holds one ends the machine.
pub struct StopWire(());

impl StopWire {
    /// Every committed record the console has not spoken onto the UART.
    pub fn drain(&mut self) {
        if serial::has_console() {
            drain_records();
        }
    }
}

/// The UART, for the stop once it has stopped userland: from a line of its
/// own, since the holder may have been stopped mid-line or behind the log.
pub fn take_for_the_stop(_stopping: &crate::quiesce::Stopping) -> StopWire {
    after_the_holder(serial::write);
    StopWire(())
}

/// Every record the console has not spoken through the registers the panic
/// path holds. Unbounded because interrupt latency doesn't matter while
/// halting, and the report should be whole.
pub fn drain_locked(guard: &mut BackendGuard) {
    after_the_holder(|bytes| guard.write_raw(bytes));
    drain_ordered(&Shards, &mut &DRAINED, &mut Spoken(|bytes: &[u8]| guard.write_raw(bytes)));
}

/// Drain straight to the UART, over whoever holds its registers.
///
/// # Safety
/// Panic path only, once `serial::panic_flush` found no clean handoff; the position is unsynchronised and a record may reach the wire twice.
pub unsafe fn drain_bypassed(uart: &mut PanicUart) {
    after_the_holder(|bytes| uart.write(bytes));
    drain_ordered(&Shards, &mut &DRAINED, &mut Spoken(|bytes: &[u8]| uart.write(bytes)));
}

/// What the stop and a fatal path start with where the UART was its
/// holder's: a line of their own, which the holder may have left unended,
/// saying the kernel's lines follow ([`toyos_abi::log::KERNEL_TAKES_THE_UART`]).
fn after_the_holder(mut write: impl FnMut(&[u8])) {
    if !inline() {
        write(b"\n");
        write(toyos_abi::log::KERNEL_TAKES_THE_UART.as_bytes());
        write(b"\n");
    }
}

/// Each record a drain walks, onto its output.
struct Spoken<F: FnMut(&[u8])>(F);

impl<F: FnMut(&[u8])> RecordSink for Spoken<F> {
    fn put(&mut self, record: &LogRecord) -> bool {
        write_line(record, &mut self.0);
        true
    }
}

/// Sized for the tag, the ABI's widest bracket, the message, and the elision note.
/// A buffer, not a bound: an overlong line spills to the backend under the same guard instead of truncating.
const LINE_BYTES: usize = toyos_abi::log::MAX_RECORD_MESSAGE + 160;

/// One rendered line on its way to the backend.
/// Buffered: writing each `Display` fragment straight through would cost one host round trip per fragment.
struct Line<F: FnMut(&[u8])> {
    emit: F,
    buf: [u8; LINE_BYTES],
    len: usize,
}

impl<F: FnMut(&[u8])> Line<F> {
    fn new(emit: F) -> Self {
        Self { emit, buf: [0; LINE_BYTES], len: 0 }
    }

    fn flush(&mut self) {
        if self.len > 0 {
            (self.emit)(&self.buf[..self.len]);
            self.len = 0;
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(LINE_BYTES) {
            if self.len + chunk.len() > LINE_BYTES {
                self.flush();
            }
            self.buf[self.len..self.len + chunk.len()].copy_from_slice(chunk);
            self.len += chunk.len();
        }
    }

    fn finish(mut self) {
        self.push(b"\n");
        self.flush();
    }
}

impl<F: FnMut(&[u8])> core::fmt::Write for Line<F> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.push(s.as_bytes());
        Ok(())
    }
}

/// Render one record as the console line; `logkeeper`'s `/log` sink renders the same line with its wall clock.
pub fn write_line(record: &LogRecord, emit: impl FnMut(&[u8])) {
    use core::fmt::Write;
    let mut line = Line::new(emit);
    let _ = write!(line, "{record}");
    line.finish();
}
