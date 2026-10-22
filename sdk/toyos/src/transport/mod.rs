//! The transport every client/server session runs over.
//!
//! **A session is one connection and, at most, one region.** The connection
//! carries the hello, handles, doorbells and the hang-up; the region carries
//! [`Producer`]/[`Consumer`] queues of fixed-size entries and an arena of
//! [`Run`]s, or byte [`stream`]s and the wants that say when to ring, two of
//! which with their `end`s are a duplex [`session`]. A protocol says what the
//! entries and bytes mean; this module says only what is safe.
//!
//! **Nothing here holds the region or makes a syscall.** An adapter, such as
//! [`crate::net::stream`], hands every call the `N` [`Word`]s an end was made
//! over, and copies bytes itself, through a run's [`Span`], once: no reference
//! to the peer's bytes is formed.
//!
//! **What the peer writes is untrusted until decoded.** An entry comes out as
//! the peer's words, for the protocol's decoder to bound; a peer's cursor is
//! bounded against the ring before it is believed, and the variant of
//! [`Violation`] names what it claimed; a run is bounded against the
//! [`Geometry`], and a tag against the [`Inflight`] table. An end keeps its
//! own cursor locally and only stores the shared one, so nothing the peer
//! writes moves it, and a cursor the peer moves backwards within bounds costs
//! only the peer.
//!
//! **A restart is seen only as a hang-up**, and [`Inflight::end`] answers every
//! tag that was in flight, once; a tag from before it answers nothing.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), forbid(clippy::arithmetic_side_effects, clippy::unwrap_used, clippy::expect_used, clippy::panic))]
#![cfg_attr(not(test), deny(clippy::indexing_slicing, clippy::as_conversions))]

mod arena;
mod inflight;
mod queue;
pub mod session;
pub mod stream;

use core::sync::atomic::{AtomicU32, Ordering};

pub use arena::{Geometry, Run, Span};
pub use inflight::Inflight;
pub use queue::{Consumer, Place, Producer};

/// One shared 32-bit word of a region: an atomic over the mapping, or a
/// model's.
pub trait Word {
    fn load(&self, order: Ordering) -> u32;
    fn store(&self, value: u32, order: Ordering);
    fn swap(&self, value: u32, order: Ordering) -> u32;
    fn compare_exchange(&self, current: u32, new: u32, success: Ordering, failure: Ordering) -> Result<u32, u32>;
}

impl Word for AtomicU32 {
    fn load(&self, order: Ordering) -> u32 {
        AtomicU32::load(self, order)
    }
    fn store(&self, value: u32, order: Ordering) {
        AtomicU32::store(self, value, order)
    }
    fn swap(&self, value: u32, order: Ordering) -> u32 {
        AtomicU32::swap(self, value, order)
    }
    fn compare_exchange(&self, current: u32, new: u32, success: Ordering, failure: Ordering) -> Result<u32, u32> {
        AtomicU32::compare_exchange(self, current, new, success, failure)
    }
}

/// How an end stores the cursor that hands what it wrote to the peer.
#[cfg(not(feature = "transport-publish-relaxed"))]
const PUBLISH: Ordering = Ordering::Release;
#[cfg(feature = "transport-publish-relaxed")]
const PUBLISH: Ordering = Ordering::Relaxed;

/// Word `at` of an end's words, and the one index into them: every `at` an end
/// forms is below `N`, because its place is and a ring position is reduced
/// below the depth.
#[allow(clippy::indexing_slicing)]
fn word<W, const N: usize>(page: &[W; N], at: usize) -> &W {
    &page[at]
}

/// A distance the peer's cursor claims, believed only up to `cap`.
fn clamp(claimed: u32, cap: u32, broken: Violation) -> Result<u32, Violation> {
    let cap = if cfg!(feature = "transport-no-clamp") { u32::MAX } else { cap };
    if claimed > cap { Err(broken) } else { Ok(claimed) }
}

/// The peer wrote what no peer of the protocol writes. The session is over;
/// the variant is its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Violation {
    /// A producer's tail more than the ring holds past what was released.
    TailPastDepth,
    /// A consumer's head past what was published, or more than the ring holds
    /// behind it.
    HeadPastTail,
    /// A reply no server of the protocol writes.
    Entry,
    /// A run outside the arena.
    Run,
    /// A tag nothing is in flight under.
    Tag,
    /// This end's own cursor, as another holder of the end stored it, names no
    /// position.
    OwnCursor,
}
