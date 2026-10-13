//! The kernel's keyboard stream: each USB keyboard's key transitions, as a
//! HID usage and a direction, queued as they come for the claim's holder.
//!
//! **One source to the owner of input, merged nowhere here.** Two keyboards
//! holding one usage queue two presses; the owner keeps this stream's held
//! set and every other source's (`toyos_input::Merge`), and makes the
//! modifier mask. Layout, dead keys and escape sequences are
//! `toyos-keymap`'s, in userland.

use alloc::collections::VecDeque;

use crate::sync::Lock;
use crate::watch::Watch;
pub use toyos_abi::input::{RawKeyEvent, MOD_RELEASED};

static KEY_BUF: Lock<VecDeque<RawKeyEvent>> = Lock::new(VecDeque::new());

/// What a keyboard read and a keyboard poll wait on.
pub static WATCH: Watch = Watch::new();

/// How many transitions the kernel holds for a reader that is not reading; the oldest is dropped on overflow.
pub const MAX_QUEUED_EVENTS: usize = 512;

/// Throw away everything queued; called when the device changes hands so no claimant inherits another's keystrokes.
pub fn discard_queued() {
    KEY_BUF.lock().clear();
}

/// Queue the transitions one USB keyboard's report made, and return how many queued.
pub fn apply(transitions: toyos_usbhid::keyboard::Transitions) -> usize {
    let mut buf = KEY_BUF.lock();
    let mut queued = 0;
    for key in transitions {
        if buf.len() >= MAX_QUEUED_EVENTS {
            buf.pop_front();
        }
        let modifiers = if key.pressed { 0 } else { MOD_RELEASED };
        buf.push_back(RawKeyEvent { keycode: key.usage, modifiers });
        queued += 1;
    }
    queued
}

pub fn has_data() -> bool {
    !KEY_BUF.lock().is_empty()
}

pub fn try_read_event() -> Option<RawKeyEvent> {
    KEY_BUF.lock().pop_front()
}
