//! The kernel's console wire, the UART, which the kernel writes only where no
//! process can: inline, as each record commits, until [`hand_over`] gives it
//! to its claim's holder before userland runs; from the moment the stop has
//! stopped userland to the machine's end ([`StopWire`]); and on a fatal path
//! ([`drain_locked`], [`drain_bypassed`]). Between the hand-over and either
//! end the UART is the holder's, which `logkeeper` feeds, and no record of
//! the kernel's touches it.
//!
//! **The console has spoken every record stamped at or before [`spoken`]**:
//! what the inline drain put on the UART, and from the hand-over what
//! `logkeeper` says the console's channels have delivered ([`speak`]). The
//! stop and a fatal path put every record stamped after it on the UART, so
//! they repeat at most what a channel delivered and had not said yet, and
//! lose none of the kernel's.
//!
//! **One writer at a time**: the boot's, whose drains take turns through a
//! claim on the wire; then the holder's; then the stop's, once userland is
//! stopped, or a fatal path's, once every other CPU is. A holder the stop did
//! not stop within its budget, which the stop's record names, may still write
//! over it.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use toyos_abi::log::LogRecord;

use crate::drivers::serial::{self, BackendGuard, PanicUart};

use super::read::{drain_ordered, Published, RecordSink, Shards};

/// Whether the UART is its claim's holder's; set once, at [`hand_over`].
static HANDED: AtomicBool = AtomicBool::new(false);

/// One position for every drain context, so a record never reaches the wire
/// twice: moved by the inline drain, then by the stop and the fatal paths.
static DRAINED: Published = Published::new();

/// The newest stamp the console has spoken: see the module's header.
static SPOKEN: AtomicU64 = AtomicU64::new(0);

/// A boot drain's claim on the wire, which a drain that finds it held leaves
/// to its holder: that drain walks to the newest commit before letting go.
static WIRE: AtomicBool = AtomicBool::new(false);

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
/// so far: the boot's last inline drain. The boot CPU's, once, before
/// userland can claim the UART.
pub fn hand_over() {
    drain_inline();
    HANDED.store(true, Ordering::Release);
}

/// Every committed record onto the UART, through its registers a burst at a
/// time, and the newest spoken stamp moved past each.
fn drain_records() {
    struct Wire;
    impl RecordSink for Wire {
        fn put(&mut self, record: &LogRecord) -> bool {
            write_line(record, serial::write);
            SPOKEN.fetch_max(record.at_ns, Ordering::Relaxed);
            true
        }
    }
    drain_ordered(&Shards, &mut &DRAINED, &mut Wire);
}

/// The newest stamp the console has spoken.
pub fn spoken() -> u64 {
    SPOKEN.load(Ordering::Relaxed)
}

/// The console's channels have delivered every record stamped at or before
/// `at_ns`, as `logkeeper` counts them; answers the newest spoken stamp,
/// which never moves back, nor past the clock: no record committed after the
/// word, a fatal path's among them, is kept off the UART by it.
pub fn speak(at_ns: u64) -> u64 {
    let at_ns = at_ns.min(crate::clock::stamp().map_or(0, |now| now.nanos()));
    SPOKEN.fetch_max(at_ns, Ordering::Relaxed).max(at_ns)
}

/// The console as the stop holds it, from the moment userland has stopped to
/// the machine's end. Never dropped: what holds one ends the machine.
pub struct StopWire(());

impl StopWire {
    /// Every committed record the console has not spoken onto the UART.
    pub fn drain(&mut self) {
        if !serial::has_console() {
            return;
        }
        let mut sink = Unspoken { spoken: Some(spoken()), out: serial::write };
        drain_ordered(&Shards, &mut &DRAINED, &mut sink);
    }
}

/// The UART, for the stop once it has stopped userland: the holder's last
/// line ended, so the stop's first is a line of its own.
pub fn take_for_the_stop() -> StopWire {
    if !inline() {
        serial::write(b"\n");
    }
    StopWire(())
}

/// Every record the console has not spoken through the registers the panic
/// path holds. Unbounded because interrupt latency doesn't matter while
/// halting, and the report should be whole.
pub fn drain_locked(guard: &mut BackendGuard) {
    let spoken = after_the_holder(|bytes| guard.write_raw(bytes));
    drain_ordered(&Shards, &mut &DRAINED, &mut Unspoken { spoken, out: |bytes: &[u8]| guard.write_raw(bytes) });
}

/// Drain straight to the UART, over whoever holds its registers.
///
/// # Safety
/// Panic path only, once `serial::panic_flush` found no clean handoff; the position is unsynchronised and a record may reach the wire twice.
pub unsafe fn drain_bypassed(uart: &mut PanicUart) {
    let spoken = after_the_holder(|bytes| uart.write(bytes));
    drain_ordered(&Shards, &mut &DRAINED, &mut Unspoken { spoken, out: |bytes: &[u8]| uart.write(bytes) });
}

/// What a fatal path starts with: a line of its own, where a holder may have
/// left one unended, and the newest stamp spoken; before the hand-over the
/// position alone says what is spoken, an untimed boot's records among it.
fn after_the_holder(mut write: impl FnMut(&[u8])) -> Option<u64> {
    if inline() {
        return None;
    }
    write(b"\n");
    Some(spoken())
}

/// Records stamped after `spoken` onto `out`; the rest advance the position
/// with no byte built.
struct Unspoken<F: FnMut(&[u8])> {
    spoken: Option<u64>,
    out: F,
}

impl<F: FnMut(&[u8])> RecordSink for Unspoken<F> {
    fn put(&mut self, record: &LogRecord) -> bool {
        if self.spoken.is_none_or(|spoken| record.at_ns > spoken) {
            write_line(record, &mut self.out);
        }
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
