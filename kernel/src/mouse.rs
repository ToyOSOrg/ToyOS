use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU8, AtomicU16, AtomicU64, Ordering};
use crate::inbox::InboxId;
use crate::sync::Lock;
pub use toyos_abi::input::MouseEvent;

static MOUSE_BUF: Lock<VecDeque<MouseEvent>> = Lock::new(VecDeque::new());

/// How many pointer updates the kernel holds for a reader that is not reading.
pub const MAX_QUEUED_EVENTS: usize = 512;
static LAST_X: AtomicU16 = AtomicU16::new(0);
static LAST_Y: AtomicU16 = AtomicU16::new(0);
static INBOX_WATCHERS: Lock<Vec<InboxId>> = Lock::new(Vec::new());

/// Which physical pointer a report came from, keyed by device and not by bus.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PointerSource(u8);

/// One bit per entry of `BUTTONS`, permanently set for bit 0 (the i8042's aux port).
static IN_USE: [AtomicU64; 4] = [
    AtomicU64::new(1),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];

impl PointerSource {
    /// The i8042's aux port, of which a machine has at most one.
    pub const PS2: Self = Self(0);

    /// The lowest free entry in the button table, or `None` when every entry is held; a caller that gets `None` must not bind the device, or the two devices' buttons flap.
    pub fn claim() -> Option<Self> {
        for (word, bits) in IN_USE.iter().enumerate() {
            let mut seen = bits.load(Ordering::Relaxed);
            while seen != u64::MAX {
                let bit = seen.trailing_ones();
                match bits.compare_exchange_weak(
                    seen,
                    seen | (1 << bit),
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return Some(Self((word as u32 * 64 + bit) as u8)),
                    Err(now) => seen = now,
                }
            }
        }
        None
    }

    /// Which entry of the button table this source publishes into.
    pub fn id(self) -> u8 {
        self.0
    }
}

/// One byte per source a `PointerSource` can name.
static BUTTONS: [AtomicU8; 256] = [const { AtomicU8::new(0) }; 256];

/// OR'd across sources, so a zero-buttons report from one live pointer cannot clear a button another pointer holds.
fn merged_buttons() -> u8 {
    BUTTONS.iter().fold(0, |acc, b| acc | b.load(Ordering::Relaxed))
}

/// Where a report puts the pointer.
#[derive(Clone, Copy, Debug)]
pub enum Motion {
    Relative { dx: i32, dy: i32 },
    Absolute { x: u16, y: u16 },
}

// Per-axis, not a single scalar: the compositor's space is square and screens are not,
// so one shared scalar would skew diagonal motion by the screen's aspect ratio.
const REL_SCALE: i32 = 64;

static SCALE_X: AtomicU16 = AtomicU16::new(REL_SCALE as u16);
static SCALE_Y: AtomicU16 = AtomicU16::new(REL_SCALE as u16);

/// The per-axis scale a screen of this size wants, chosen so `x * width == y * height`.
pub fn rel_scale_for(width: u32, height: u32) -> (u16, u16) {
    let short = width.min(height) as i32;
    (
        (REL_SCALE * short / width.max(1) as i32).max(1) as u16,
        (REL_SCALE * short / height.max(1) as i32).max(1) as u16,
    )
}

/// Publish the geometry the absolute space is being mapped onto.
pub fn set_screen(width: u32, height: u32) {
    if width == 0 || height == 0 {
        return;
    }
    let (x, y) = rel_scale_for(width, height);
    SCALE_X.store(x, Ordering::Relaxed);
    SCALE_Y.store(y, Ordering::Relaxed);
    crate::log!("mouse: rel scale x={} y={} (screen {}x{})", x, y, width, height);
}

pub fn add_inbox_watcher(id: InboxId) {
    let mut w = INBOX_WATCHERS.lock();
    if !w.contains(&id) { w.push(id); }
}

pub fn remove_inbox_watcher(id: InboxId) {
    INBOX_WATCHERS.lock().retain(|&x| x != id);
}

pub fn inbox_watchers() -> Vec<InboxId> {
    INBOX_WATCHERS.lock().clone()
}

/// The only path that merges buttons and accumulates motion; queues an update and returns true iff one was queued.
pub fn handle_motion(
    source: PointerSource,
    buttons: u8,
    motion: Motion,
    scroll: i8,
) -> bool {
    let prev = BUTTONS[source.0 as usize].swap(buttons, Ordering::Relaxed);
    let merged = merged_buttons();

    let last_x = LAST_X.load(Ordering::Relaxed);
    let last_y = LAST_Y.load(Ordering::Relaxed);
    let (abs_x, abs_y) = match motion {
        Motion::Absolute { x, y } => (x, y),
        Motion::Relative { dx, dy } => (
            (last_x as i32 + dx * SCALE_X.load(Ordering::Relaxed) as i32).clamp(0, 32767) as u16,
            (last_y as i32 + dy * SCALE_Y.load(Ordering::Relaxed) as i32).clamp(0, 32767) as u16,
        ),
    };

    if abs_x == last_x && abs_y == last_y && scroll == 0 && buttons == prev {
        return false;
    }
    LAST_X.store(abs_x, Ordering::Relaxed);
    LAST_Y.store(abs_y, Ordering::Relaxed);
    queue(MouseEvent { buttons: merged, scroll, abs_x, abs_y });
    true
}

fn queue(event: MouseEvent) {
    let mut buf = MOUSE_BUF.lock();
    if buf.len() >= MAX_QUEUED_EVENTS {
        buf.pop_front();
    }
    buf.push_back(event);
}

/// Throw away everything queued, for the reason [`crate::keyboard::discard_queued`] gives.
pub fn discard_queued() {
    MOUSE_BUF.lock().clear();
}

/// Release everything a source was holding, and publish the result. A source that goes silent without this call leaves its button stuck down and the compositor stuck mid-drag.
pub fn release_buttons(source: PointerSource) -> bool {
    let before = merged_buttons();
    BUTTONS[source.0 as usize].store(0, Ordering::Relaxed);
    let after = merged_buttons();
    if before == after {
        return false;
    }
    queue(MouseEvent {
        buttons: after,
        scroll: 0,
        abs_x: LAST_X.load(Ordering::Relaxed),
        abs_y: LAST_Y.load(Ordering::Relaxed),
    });
    true
}

/// A pointer that is gone: release whatever it was holding and give its entry back. Returns whether the release changed the merge.
pub fn unbind(source: PointerSource) -> bool {
    assert!(
        source != PointerSource::PS2,
        "mouse: the i8042's aux port cannot be unplugged, and freeing entry 0 would let a USB \
         pointer alias it"
    );
    let published = release_buttons(source);
    // Buttons must clear before the entry frees, or the next device inherits a stale bit.
    IN_USE[source.0 as usize / 64].fetch_and(!(1u64 << (source.0 % 64)), Ordering::Relaxed);
    published
}

/// Process a HID report: 6 bytes `[buttons, x_lo, x_hi, y_lo, y_hi, scroll]` for a tablet, 3/4 bytes `[buttons, dx, dy, scroll?]` for a boot mouse. Returns the number of events queued.
pub fn handle_report(source: PointerSource, report: &[u8]) -> usize {
    let queued = if report.len() >= 6 {
        handle_motion(
            source,
            report[0],
            Motion::Absolute {
                x: u16::from_le_bytes([report[1], report[2]]),
                y: u16::from_le_bytes([report[3], report[4]]),
            },
            report[5] as i8,
        )
    } else if report.len() >= 3 {
        handle_motion(
            source,
            report[0],
            Motion::Relative { dx: report[1] as i8 as i32, dy: report[2] as i8 as i32 },
            if report.len() > 3 { report[3] as i8 } else { 0 },
        )
    } else {
        false
    };
    queued as usize
}

pub fn has_data() -> bool {
    !MOUSE_BUF.lock().is_empty()
}

pub fn try_read_event() -> Option<MouseEvent> {
    MOUSE_BUF.lock().pop_front()
}
