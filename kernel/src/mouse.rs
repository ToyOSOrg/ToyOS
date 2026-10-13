//! The kernel's mouse stream: each USB pointer's reports, queued as the
//! device gave them for the claim's holder.
//!
//! **One source to the owner of input, merged nowhere here.** Each record
//! names its pointer ([`PointerSource`]) and carries that pointer's own
//! buttons and its motion, relative or absolute; the owner ORs every
//! pointer's buttons and keeps the one position (`toyos_input::Pointer`).

use alloc::collections::VecDeque;
use crate::sync::Lock;
use crate::watch::Watch;
use kernel::pointers::Sources;
pub use toyos_abi::input::MouseEvent;
use toyos_abi::input::{POINTER_ABSOLUTE, POINTER_RELATIVE};

static MOUSE_BUF: Lock<VecDeque<MouseEvent>> = Lock::new(VecDeque::new());

/// How many pointer updates the kernel holds for a reader that is not reading.
pub const MAX_QUEUED_EVENTS: usize = 512;
/// What a pointer poll waits on; no read blocks on the mouse.
pub static WATCH: Watch = Watch::new();

/// Which physical pointer a report came from, keyed by device and not by bus.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PointerSource(u8);

static IN_USE: Sources = Sources::new();

impl PointerSource {
    /// The lowest free source, or `None` when every one is held; a caller that gets `None` must not bind the device, or two devices' buttons share one record.
    pub fn claim() -> Option<Self> {
        IN_USE.claim().map(Self)
    }

    /// What the records of this source carry as theirs.
    pub fn id(self) -> u8 {
        self.0
    }
}

/// Where a report puts the pointer.
#[derive(Clone, Copy, Debug)]
pub enum Motion {
    Relative { dx: i16, dy: i16 },
    /// Each axis at most `toyos_abi::input::POINTER_ABSOLUTE_MAX`.
    Absolute { x: u16, y: u16 },
}

/// Queue one report of `source`'s; `buttons` as `toyos_usbhid` decodes them,
/// boot-mouse order's five.
pub fn report(source: PointerSource, buttons: u8, motion: Motion, scroll: i8) {
    let (kind, x, y) = match motion {
        Motion::Relative { dx, dy } => (POINTER_RELATIVE, dx, dy),
        // At most `POINTER_ABSOLUTE_MAX`, which an `i16` holds.
        Motion::Absolute { x, y } => (POINTER_ABSOLUTE, x as i16, y as i16),
    };
    let mut buf = MOUSE_BUF.lock();
    if buf.len() >= MAX_QUEUED_EVENTS {
        buf.pop_front();
    }
    buf.push_back(MouseEvent { source: source.0, buttons, scroll, kind, x, y });
}

/// Throw away everything queued, for the reason [`crate::keyboard::discard_queued`] gives.
pub fn discard_queued() {
    MOUSE_BUF.lock().clear();
}

/// `source` holds no button now. A source that goes silent without this call leaves its button stuck down and the owner stuck mid-drag.
pub fn release_buttons(source: PointerSource) {
    report(source, 0, Motion::Relative { dx: 0, dy: 0 }, 0);
}

/// A pointer that is gone: release whatever it was holding and give its source back.
pub fn unbind(source: PointerSource) {
    release_buttons(source);
    // Released before the source frees, or the next device's records follow a stale button.
    IN_USE.give_back(source.0);
}

pub fn has_data() -> bool {
    !MOUSE_BUF.lock().is_empty()
}

pub fn try_read_event() -> Option<MouseEvent> {
    MOUSE_BUF.lock().pop_front()
}
