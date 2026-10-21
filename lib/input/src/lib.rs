//! Input, as its owner sees it: every source's keys and pointers, merged.
//!
//! **A source says what one device did and nothing more**: a key's
//! transition with no modifier mask, a pointer's own buttons and its motion
//! as the device gave it. The owner of input keeps a [`Merge`] and a
//! [`Pointer`] over every source it holds, keyed by the connection and the
//! device's `source`, so a key or a button is held while any device holds
//! it; a device's `gone` releases what it held, a connection's end what
//! every device on it held, and neither anything another holds.
//!
//! [`wire`] is the one reader of a source's records, whether they came off the
//! `input` port or the kernel's streams: what does not decode is refused by
//! name. [`keys`] is one keyboard's held set, which the kernel's USB keyboard
//! decode holds too. Nothing here touches a device, allocates or holds a lock.

#![no_std]
#![forbid(unsafe_code)]

pub mod keys;
pub mod merge;
pub mod wire;

pub use keys::Keys;
pub use merge::{Full, Merge, Pointer, Slot, State};
