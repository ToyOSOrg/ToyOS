//! `inspect`: what a running owner says about its own state, now.
//!
//! **The log is history; this is the present.** Every owner of a device or a
//! service already knows its own state — a link's speed, a stream's underruns,
//! the file the log is going to — and answers one request for it on the port it
//! already serves. `/system/bin/inspect` is the one generic reader. There is no
//! daemon in between and no registry: the reader reaches an owner only through
//! a connector its manifest row grants, exactly as any other client does, so
//! observing an owner is holding its connector and nothing becomes reachable
//! that was not before.
//!
//! This crate is everything both ends have to agree on, and none of the
//! effects:
//!
//! - [`Owner`]: which root of the path space each owner speaks for, and the port
//!   it answers on.
//! - [`MSG_INSPECT`] / [`MSG_SNAPSHOT`]: the request and the one frame that
//!   answers it.
//! - [`Snapshot`] and [`decode`]: the typed wire form of `path = value` pairs.
//! - [`Selector`]: the reader's frozen selector grammar.
//! - [`line`] and [`json`]: the two renderings.
//! - [`Invocation`]: the reader's command line.
//! - [`dev`]: the kernel's device inventory under `dev.*`, the one root no
//!   port answers for — the reader asks the kernel for it on a `SysCap`
//!   carrying `Rights::INVENTORY`.
//!
//! # The path grammar
//!
//! A path is dotted segments, each one or more of `a-z`, `0-9`, `_`, `-` and
//! `:`, at most [`MAX_PATH`] bytes in all. The first segment is the owner's
//! root, and an owner speaks for its own root and no other: a snapshot carrying
//! a path under another root is refused by [`decode`], so one owner cannot
//! answer in another's name.
//!
//! # The selector grammar, which is frozen
//!
//! A selector is a path whose segments may also be `*`. **A `*` matches one or
//! more whole segments** and nothing else: `net.*` is everything under `net`,
//! `*.errors` is every path whose last segment is `errors`, and `sound.*.max`
//! is every `max` anywhere below `sound`. A selector with no `*` matches exactly
//! the one path it spells. A `*` inside a segment (`ne*`) is refused by name
//! rather than read as a glob, so the grammar can never grow a second meaning
//! for a character it already accepts.
//!
//! # What a snapshot may carry
//!
//! **Aggregates of the owner's own state, never another client's.** A connector
//! is held by many programs — every audio client holds `soundd` — so anything a
//! snapshot carries is readable by all of them: a count of windows, never a
//! title; a count of sockets, never an endpoint.
//!
//! Pure: `core` and `alloc`, no `unsafe`, no I/O.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod dev;
mod invocation;
mod path;
mod render;
mod selector;
mod wire;

pub use invocation::{Invocation, UsageError};
pub use path::{check_path, PathError, MAX_PATH};
pub use render::{json, line};
pub use selector::{Selector, SelectorError};
pub use wire::{decode, DecodeError, EncodeError, Snapshot, Value};

/// An owner of a subtree of the path space, and the port it answers on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Owner {
    /// The first segment of every path this owner answers with.
    pub root: &'static str,
    /// The service name its manifest row `serves`, which is the connector a
    /// reader has to be granted to ask it anything.
    pub port: &'static str,
}

/// netd: the link, the lease, the card's counters and the socket table's size.
pub const NET: Owner = Owner { root: "net", port: "netd" };
/// soundd: the device it drives, its stream state and its underruns.
pub const SOUND: Owner = Owner { root: "sound", port: "soundd" };
/// logd: where this boot's log is going, and how much has gone there.
pub const LOG: Owner = Owner { root: "log", port: "log" };
/// The compositor: the panel, the windows it holds and its frame statistics.
pub const DISPLAY: Owner = Owner { root: "display", port: "compositor" };

/// Every owner the reader knows, in the order it asks them.
pub const OWNERS: [Owner; 4] = [NET, SOUND, LOG, DISPLAY];

/// The request: a bare frame header with no payload. A request that carries a
/// payload is not this protocol, and an owner drops its connection.
///
/// The value spells `insp` so it cannot collide with any owner's own small
/// message numbers, which every protocol here counts up from 1.
pub const MSG_INSPECT: u32 = u32::from_le_bytes(*b"insp");

/// The answer: one frame whose payload is a [`Snapshot`]'s encoding. The owner
/// sends exactly one and closes the connection.
pub const MSG_SNAPSHOT: u32 = u32::from_le_bytes(*b"snap");

const _: () = assert!(
    MSG_INSPECT != MSG_SNAPSHOT && MSG_INSPECT > 0xffff && MSG_SNAPSHOT > 0xffff,
    "two distinct numbers, clear of every protocol that counts up from 1"
);

/// The largest encoded snapshot, which is the SDK's `ipc::MAX_FRAME_LEN`: the
/// answer is one frame, sent in one non-blocking write. This crate cannot name
/// the SDK, so every owner and the reader assert the two equal where both are
/// in scope.
pub const MAX_SNAPSHOT_BYTES: usize = 8192;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_owner_root_is_one_valid_segment_and_distinct() {
        for (i, owner) in OWNERS.iter().enumerate() {
            check_path(owner.root).expect("a root is a path");
            assert!(!owner.root.contains('.'), "{} is one segment", owner.root);
            for other in &OWNERS[i + 1..] {
                assert_ne!(owner.root, other.root);
                assert_ne!(owner.port, other.port);
            }
        }
    }
}
