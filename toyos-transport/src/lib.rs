//! The transport every client/server session runs over.
//!
//! **A session is one connection and, at most, one region.** The connection
//! carries the hello, handles, doorbells and the hang-up; the region carries
//! [`Producer`]/[`Consumer`] queues of fixed-size entries and an arena of
//! [`Run`]s. A protocol says what the entries mean; this crate says only what
//! is safe.
//!
//! **Nothing here holds the region.** An adapter hands every call the region's
//! words as a slice of [`Word`]s and copies bytes itself, through a run's
//! [`Span`], once: no reference to the peer's bytes is formed.
//!
//! **What the peer writes is untrusted until decoded.** An entry comes out as
//! [`Untrusted`] words; a peer's cursor is bounded against the ring before it
//! is believed, and the variant of [`Violation`] names what it claimed; a run
//! is bounded against the [`Geometry`], and a tag against the [`Inflight`]
//! table. An end keeps its own cursor locally and only stores the shared one,
//! so nothing the peer writes moves it, and a cursor the peer moves backwards
//! within bounds costs only the peer.
//!
//! **Publication is `Release`/`Acquire`; the wake is a pair of `SeqCst`
//! fences.** A consumer that finds nothing stores its `sleep` word, fences and
//! looks again ([`Consumer::before_sleep`]); a producer stores its tail,
//! fences and loads `sleep`, and asks for a wake ([`Wake::Peer`]) only if it is
//! set. A busy consumer costs a producer no syscall, and a hostile `sleep` only
//! costs a wake.
//!
//! **A restart is seen only as a hang-up**, and [`Inflight::end`] answers every
//! tag that was in flight, once; a tag from before it answers nothing.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
#![cfg_attr(not(test), forbid(clippy::arithmetic_side_effects, clippy::indexing_slicing, clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::as_conversions))]

mod arena;
mod inflight;
#[cfg(test)]
mod model;
mod queue;

use core::sync::atomic::{AtomicU32, Ordering};

pub use arena::{Geometry, Run, Span};
pub use inflight::Inflight;
pub use queue::{Consumer, Place, Producer};
pub use toyos_untrusted::Untrusted;

/// One shared 32-bit word of a region: an atomic over the mapping, or a
/// model's.
pub trait Word {
    fn load(&self, order: Ordering) -> u32;
    fn store(&self, value: u32, order: Ordering);
    /// A `SeqCst` fence, in the memory model this word lives in.
    fn fence();
}

impl Word for AtomicU32 {
    fn load(&self, order: Ordering) -> u32 {
        AtomicU32::load(self, order)
    }
    fn store(&self, value: u32, order: Ordering) {
        AtomicU32::store(self, value, order)
    }
    fn fence() {
        core::sync::atomic::fence(Ordering::SeqCst)
    }
}

/// The peer wrote what no peer of the protocol writes, or the region is not
/// one this session can use. The session is over; the variant is its name.
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
    /// A place outside the words given.
    Region,
}

/// What a publish asks of the adapter.
#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Wake {
    /// The consumer said it sleeps: wake it.
    Peer,
    /// The consumer is awake and will find what was published.
    Busy,
}

/// A consumer that found nothing and said so: it waits while word `word`
/// holds `value`, as a futex does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Asleep {
    pub word: usize,
    pub value: u32,
}

/// Where a ring's two cursors and its consumer's `sleep` word are, in words.
/// The producer stores `tail`, the consumer `head` and `sleep`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Cursors {
    pub head: usize,
    pub tail: usize,
    pub sleep: usize,
}

#[cfg(not(feature = "publish-relaxed"))]
const PUBLISH: Ordering = Ordering::Release;
#[cfg(feature = "publish-relaxed")]
const PUBLISH: Ordering = Ordering::Relaxed;

fn word<W>(page: &[W], at: usize) -> Result<&W, Violation> {
    page.get(at).ok_or(Violation::Region)
}

/// A distance the peer's cursor claims, believed only up to `cap`.
fn clamp(claimed: Untrusted<u32>, cap: u32, broken: Violation) -> Result<u32, Violation> {
    let cap = if cfg!(feature = "no-clamp") { u32::MAX } else { cap };
    claimed.at_most(u64::from(cap)).ok().and_then(|n| u32::try_from(n).ok()).ok_or(broken)
}

impl Cursors {
    /// How far past `released` the producer's tail is: at most `cap`.
    fn published<W: Word>(&self, page: &[W], released: u32, cap: u32) -> Result<u32, Violation> {
        let tail = word(page, self.tail)?.load(Ordering::Acquire);
        clamp(Untrusted::new(tail).map(|t| t.wrapping_sub(released)), cap, Violation::TailPastDepth)
    }

    /// How far behind `published` the consumer's head is: at most `cap`.
    fn unreleased<W: Word>(&self, page: &[W], published: u32, cap: u32) -> Result<u32, Violation> {
        let head = word(page, self.head)?.load(Ordering::Acquire);
        clamp(Untrusted::new(head).map(|h| published.wrapping_sub(h)), cap, Violation::HeadPastTail)
    }

    fn publish<W: Word>(&self, page: &[W], tail: u32) -> Result<Wake, Violation> {
        word(page, self.tail)?.store(tail, PUBLISH);
        self.wake(page)
    }

    /// The producer's half of the wake, after what it published is stored.
    fn wake<W: Word>(&self, page: &[W]) -> Result<Wake, Violation> {
        #[cfg(not(feature = "no-wake-fence"))]
        W::fence();
        Ok(match word(page, self.sleep)?.load(Ordering::Relaxed) {
            0 => Wake::Busy,
            _ => Wake::Peer,
        })
    }

    /// The consumer's half: say it sleeps, before it looks again.
    fn sleep<W: Word>(&self, page: &[W]) -> Result<(), Violation> {
        word(page, self.sleep)?.store(1, Ordering::Relaxed);
        #[cfg(not(feature = "no-sleep-fence"))]
        W::fence();
        Ok(())
    }

    fn awake<W: Word>(&self, page: &[W]) -> Result<(), Violation> {
        word(page, self.sleep)?.store(0, Ordering::Relaxed);
        Ok(())
    }
}
