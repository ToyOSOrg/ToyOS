//! The machine's single logical keyboard: every driver's key transitions
//! land here as a HID usage, a direction and the modifier mask across every
//! keyboard. Layout, dead keys and escape sequences are `toyos-keymap`'s, in
//! userland.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use crate::inbox::InboxId;
use crate::sync::Lock;
pub use toyos_abi::input::{RawKeyEvent, MOD_SHIFT, MOD_CTRL, MOD_ALT, MOD_GUI, MOD_RELEASED};

static KEY_BUF: Lock<VecDeque<RawKeyEvent>> = Lock::new(VecDeque::new());

/// Whether any driver that can ever feed this stream exists — the i8042's
/// keyboard armed, or an xHCI controller bound (hot-plug). A claim's evidence.
static SOURCE_EXISTS: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub fn declare_source() {
    SOURCE_EXISTS.store(true, core::sync::atomic::Ordering::Relaxed);
}

pub fn source_exists() -> bool {
    SOURCE_EXISTS.load(core::sync::atomic::Ordering::Relaxed)
}
static INBOX_WATCHERS: Lock<Vec<InboxId>> = Lock::new(Vec::new());

/// How many transitions the kernel holds for a reader that is not reading; the oldest is dropped on overflow.
pub const MAX_QUEUED_EVENTS: usize = 512;

/// Ctrl+Alt+D is recorded here, not acted on; the scheduler pass consumes it with no driver lock held.
static DUMP_REQUESTED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Consume a pending Ctrl+Alt+D. Called from `drain_irqs` and nowhere else.
pub fn take_dump_request() -> bool {
    DUMP_REQUESTED.swap(false, core::sync::atomic::Ordering::Relaxed)
}

/// Which HID usages are down, one bit each, across every keyboard; keyed by usage, so releasing one keyboard's modifier drops it even if another still holds it.
static HELD: Lock<[u64; 4]> = Lock::new([0; 4]);

pub fn add_inbox_watcher(id: InboxId) {
    let mut w = INBOX_WATCHERS.lock();
    if !w.contains(&id) { w.push(id); }
}

pub fn remove_inbox_watcher(id: InboxId) {
    INBOX_WATCHERS.lock().retain(|&x| x != id);
}

/// Wake every thread blocked on keyboard input.
pub fn wake_waiters() {
    crate::sched::waitqs::wake_device(&crate::sched::waitqs::KEYBOARD_WATCH);
}

pub fn inbox_watchers() -> Vec<InboxId> {
    INBOX_WATCHERS.lock().clone()
}

fn is_held(held: &[u64; 4], usage: u8) -> bool {
    held[usage as usize / 64] & (1 << (usage % 64)) != 0
}

fn modifiers_of(held: &[u64; 4]) -> u8 {
    let m = |a: u8, b: u8| is_held(held, a) || is_held(held, b);
    (if m(0xE1, 0xE5) { MOD_SHIFT } else { 0 })
        | (if m(0xE0, 0xE4) { MOD_CTRL } else { 0 })
        | (if m(0xE2, 0xE6) { MOD_ALT } else { 0 })
        | (if m(0xE3, 0xE7) { MOD_GUI } else { 0 })
}

/// The held-modifier bitmask; test-only, no shipping caller.
#[cfg(feature = "boot-actuators")]
pub fn modifiers() -> u8 {
    modifiers_of(&HELD.lock())
}

/// Queue one key transition; a transition to an already-held state queues nothing.
pub fn handle_key(usage: u8, pressed: bool) -> bool {
    // Sole path into KEY_BUF; splitting per driver would miss a Ctrl+Alt+D pressed across two devices.
    if usage == 0 {
        return false;
    }
    let modifiers = {
        let mut held = HELD.lock();
        if is_held(&held, usage) == pressed {
            return false;
        }
        let word = &mut held[usage as usize / 64];
        let bit = 1u64 << (usage % 64);
        if pressed { *word |= bit } else { *word &= !bit }
        modifiers_of(&held)
    };

    // Ctrl+Alt+D: keyed by HID usage so it is the same three keys under every layout; recorded, not run, since the caller holds its driver's guard.
    if pressed && modifiers & MOD_CTRL != 0 && modifiers & MOD_ALT != 0 && usage == 0x07 {
        DUMP_REQUESTED.store(true, core::sync::atomic::Ordering::Relaxed);
        return false;
    }

    let mut buf = KEY_BUF.lock();
    if buf.len() >= MAX_QUEUED_EVENTS {
        buf.pop_front();
    }
    buf.push_back(RawKeyEvent {
        keycode: usage,
        modifiers: if pressed { modifiers } else { modifiers | MOD_RELEASED },
    });
    true
}

/// Throw away everything queued; called when the device changes hands so no claimant inherits another's keystrokes.
pub fn discard_queued() {
    KEY_BUF.lock().clear();
}

/// Synthesise a release for every held usage; recovers a keyboard that reset without releasing its keys.
pub fn release_all() -> usize {
    let held = *HELD.lock();
    let mut n = 0;
    for usage in 0..=u8::MAX {
        if is_held(&held, usage) && handle_key(usage, false) {
            n += 1;
        }
    }
    n
}

/// Process a HID boot protocol keyboard report (8 bytes) and return events queued; `prev` must be this device's own last report.
pub fn handle_report(state: &mut [u8; 8], report: &[u8]) -> usize {
    let prev = *state;
    state.copy_from_slice(&report[..8]);
    let mut queued = 0;

    // report[0] carries modifiers as a bitmask, not usages; synthesized here as discrete per-modifier events.
    const MOD_BITS: [(u8, u8); 8] = [
        (0x01, 0xE0),
        (0x02, 0xE1),
        (0x04, 0xE2),
        (0x08, 0xE3),
        (0x10, 0xE4),
        (0x20, 0xE5),
        (0x40, 0xE6),
        (0x80, 0xE7),
    ];
    for &(bit, usage) in &MOD_BITS {
        let now = report[0] & bit != 0;
        if (prev[0] & bit != 0) != now && handle_key(usage, now) {
            queued += 1;
        }
    }

    for &usage in &prev[2..8] {
        if usage >= 4 && !report[2..8].contains(&usage) && handle_key(usage, false) {
            queued += 1;
        }
    }

    for &usage in &report[2..8] {
        if usage >= 4 && !prev[2..8].contains(&usage) && handle_key(usage, true) {
            queued += 1;
        }
    }

    queued
}

pub fn has_data() -> bool {
    !KEY_BUF.lock().is_empty()
}

pub fn try_read_event() -> Option<RawKeyEvent> {
    KEY_BUF.lock().pop_front()
}
