//! Everything the desktop decides, as pure functions over pure state.
//!
//! Where a window is, what changed since the last frame, what the pointer is
//! over, who has the keyboard, what a key means, what the clipboard's watcher
//! is sent, what is visible inside a damaged region and which pixels of a
//! client's buffer reach the screen. No
//! I/O, no drawing, no shared memory, no connections — those are the
//! compositor's, and it is the only caller.
//!
//! The split exists because a QEMU boot is a poor way to ask any of these
//! questions. Every decision that can be a pure function is one the host
//! exercises in milliseconds — the arithmetic that decides what a frame costs
//! most of all — and the guest tests are left to certify what only a guest
//! can: that the right pixels reached a panel.
//!
//! A window carries its client as an opaque `C` ([`Window`]), so reordering the
//! stack cannot separate a window's geometry from the connection behind it.
//! Nothing in this crate ever looks at that value.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod budget;
pub mod clipboard;
pub mod cursor;
pub mod damage;
pub mod hit;
pub mod input;
pub mod layout;
pub mod mode;
pub mod plan;
pub mod rect;
pub mod scale;
pub mod signout;
pub mod stack;
pub mod taskbar;
pub mod volume;
pub mod window;

pub use budget::{create_verdict, requested, Requested, max_windows, pending_admits, window_bytes, Port, Verdict};
pub use damage::{Damage, MAX_DAMAGE_RECTS};
pub use hit::{hit_test, Hit};
pub use input::{
    cursor_from_abs, cursor_style, edge_snap, fold_mouse, key_action, tab_action, CursorStyle, Grab,
    Held, KeyAction, MouseSample, PromptGate, Released, SignOutKey, TabAction, DRAG_THRESHOLD,
    sign_out_key,
};
pub use layout::{relayout, set_mode, Chrome, Desk, TEXT_CELL};
pub use mode::Mode;
pub use plan::{compose, content_blit, Blit, Layer};
pub use rect::{Point, Rect};
pub use scale::Scale;
pub use signout::{sign_out_rect, sign_out_text, Opening, SIGN_OUT_CHARS};
pub use stack::Stack;
pub use taskbar::{Taskbar, MAX_STATUS_CHARS};
pub use window::{Buffers, Level, Presented, Window, WindowId, WindowMode};

/// The colour of the frame the compositor draws around a prompt and around
/// nothing else it draws: a cue to the person at the screen, and what a
/// screendump finds the prompt by. A client can paint it too; what an answer
/// grants is decided by who asked, not by how it looked
/// (`toyos_manifest::consent`).
pub const PROMPT_FRAME: [u8; 3] = [0xf5, 0xb0, 0x2e];
