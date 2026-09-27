//! A single-producer queue of `E`-word entries, `D` deep, among `N` words.
//!
//! **A producer writes an entry's words, then publishes the tail with
//! `Release`; a consumer loads the tail with `Acquire`, then reads the words.**
//! The head goes back the other way. Each end looks at the peer's cursor when
//! what it last saw is spent, not once per entry, and stores its own once per
//! batch.

use core::sync::atomic::Ordering;

use crate::{Untrusted, Violation, Word};

#[cfg(not(feature = "publish-relaxed"))]
const PUBLISH: Ordering = Ordering::Release;
#[cfg(feature = "publish-relaxed")]
const PUBLISH: Ordering = Ordering::Relaxed;

/// Word `at` of an end's words, and the one index into them: every `at` an end
/// forms is below `N`, because its [`Place`] is and a ring position is masked
/// below the depth.
#[allow(clippy::indexing_slicing)]
fn word<W, const N: usize>(page: &[W; N], at: usize) -> &W {
    &page[at]
}

/// A distance the peer's cursor claims, believed only up to `cap`.
fn clamp(claimed: Untrusted<u32>, cap: u32, broken: Violation) -> Result<u32, Violation> {
    let cap = if cfg!(feature = "no-clamp") { u32::MAX } else { cap };
    claimed.at_most(u64::from(cap)).ok().and_then(|n| u32::try_from(n).ok()).ok_or(broken)
}

/// Where a queue of `E`-word entries, `D` deep, is among `N` words: the word
/// its consumer stores its head in, the word its producer stores its tail in,
/// and its first entry.
///
/// ```
/// use toyos_transport::Place;
/// const QUEUE: Place<2, 4, 10> = Place::new::<0, 1, 2>();
/// ```
///
/// A place with a word at `N` or past it does not compile:
///
/// ```compile_fail,E0080
/// use toyos_transport::Place;
/// const QUEUE: Place<2, 4, 10> = Place::new::<0, 1, 3>();
/// ```
///
/// ```compile_fail,E0080
/// use toyos_transport::Place;
/// const QUEUE: Place<2, 4, 10> = Place::new::<10, 1, 2>();
/// ```
///
/// ```compile_fail,E0080
/// use toyos_transport::Place;
/// const QUEUE: Place<2, 4, 10> = Place::new::<0, 10, 2>();
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Place<const E: usize, const D: u32, const N: usize> {
    head: usize,
    tail: usize,
    entries: usize,
}

impl<const E: usize, const D: u32, const N: usize> Place<E, D, N> {
    pub const fn new<const HEAD: usize, const TAIL: usize, const ENTRIES: usize>() -> Self {
        const {
            assert!(D.is_power_of_two() && E > 0, "a queue is a power of two deep, of entries of a word or more");
            assert!(usize::BITS >= u32::BITS, "a ring position is a u32");
            #[allow(clippy::as_conversions)]
            let entries_end = ENTRIES + D as usize * E;
            assert!(HEAD < N && TAIL < N && entries_end <= N, "a place names a word past its words");
        }
        Self { head: HEAD, tail: TAIL, entries: ENTRIES }
    }

    /// The words of the entry at ring position `at`.
    fn entry<'a, W>(&self, page: &'a [W; N], at: u32) -> impl Iterator<Item = &'a W> {
        // `usize` holds every `u32`, so the slot is exact.
        #[allow(clippy::as_conversions)]
        let slot = (at & D.wrapping_sub(1)) as usize;
        let first = self.entries.wrapping_add(slot.wrapping_mul(E));
        (0..E).map(move |k| word(page, first.wrapping_add(k)))
    }

    /// How far past `released` the producer's tail is: at most `D`.
    fn published<W: Word>(&self, page: &[W; N], released: u32) -> Result<u32, Violation> {
        let tail = word(page, self.tail).load(Ordering::Acquire);
        clamp(Untrusted::new(tail).map(|t| t.wrapping_sub(released)), D, Violation::TailPastDepth)
    }

    /// How far behind `published` the consumer's head is: at most `D`.
    fn unreleased<W: Word>(&self, page: &[W; N], published: u32) -> Result<u32, Violation> {
        let head = word(page, self.head).load(Ordering::Acquire);
        clamp(Untrusted::new(head).map(|h| published.wrapping_sub(h)), D, Violation::HeadPastTail)
    }
}

/// The end of a queue that writes entries. It holds its cursors and not the
/// region; every call is given the region's words.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Producer<const E: usize, const D: u32, const N: usize> {
    place: Place<E, D, N>,
    local: u32,
    published: u32,
    /// Entries that may be pushed before the head is looked at again.
    room: u32,
}

impl<const E: usize, const D: u32, const N: usize> Producer<E, D, N> {
    /// This end of the queue at `place`, its tail stored 0.
    pub fn new<W: Word>(page: &[W; N], place: Place<E, D, N>) -> Self {
        word(page, place.tail).store(0, Ordering::Release);
        Self { place, local: 0, published: 0, room: D }
    }

    /// How many entries may be pushed before the consumer frees more.
    pub fn space<W: Word>(&mut self, page: &[W; N]) -> Result<u32, Violation> {
        let unreleased = self.place.unreleased(page, self.published)?;
        let pending = self.local.wrapping_sub(self.published);
        self.room = D.saturating_sub(pending.saturating_add(unreleased));
        Ok(self.room)
    }

    /// Write one entry, or answer `false` for a full queue and write nothing.
    /// It is the consumer's once [`Self::publish`] runs.
    pub fn push<W: Word>(&mut self, page: &[W; N], words: [u32; E]) -> Result<bool, Violation> {
        if self.room == 0 && self.space(page)? == 0 {
            return Ok(false);
        }
        for (shared, value) in self.place.entry(page, self.local).zip(words) {
            shared.store(value, Ordering::Relaxed);
        }
        self.local = self.local.wrapping_add(1);
        self.room = self.room.wrapping_sub(1);
        Ok(true)
    }

    /// Publish every entry pushed so far; `false` if there was none.
    pub fn publish<W: Word>(&mut self, page: &[W; N]) -> bool {
        if self.published == self.local {
            return false;
        }
        word(page, self.place.tail).store(self.local, PUBLISH);
        self.published = self.local;
        true
    }
}

/// The end of a queue that reads entries; like [`Producer`], it holds cursors
/// and is given the region's words.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Consumer<const E: usize, const D: u32, const N: usize> {
    place: Place<E, D, N>,
    local: u32,
    released: u32,
    /// Entries published and not yet popped, as last seen.
    ready: u32,
}

impl<const E: usize, const D: u32, const N: usize> Consumer<E, D, N> {
    /// This end of the queue at `place`, its head stored 0.
    pub fn new<W: Word>(page: &[W; N], place: Place<E, D, N>) -> Self {
        word(page, place.head).store(0, Ordering::Release);
        Self { place, local: 0, released: 0, ready: 0 }
    }

    /// The next published entry, or `None` for none.
    pub fn pop<W: Word>(&mut self, page: &[W; N]) -> Result<Option<[Untrusted<u32>; E]>, Violation> {
        if self.ready == 0 {
            let published = self.place.published(page, self.released)?;
            self.ready = published.saturating_sub(self.local.wrapping_sub(self.released));
            if self.ready == 0 {
                return Ok(None);
            }
        }
        let mut words = [Untrusted::new(0); E];
        for (out, shared) in words.iter_mut().zip(self.place.entry(page, self.local)) {
            *out = Untrusted::new(shared.load(Ordering::Relaxed));
        }
        self.local = self.local.wrapping_add(1);
        self.ready = self.ready.wrapping_sub(1);
        Ok(Some(words))
    }

    /// Give every entry popped so far back to the producer.
    pub fn release<W: Word>(&mut self, page: &[W; N]) {
        if self.released != self.local {
            word(page, self.place.head).store(self.local, Ordering::Release);
            self.released = self.local;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::sync::atomic::AtomicU32;

    const D: u32 = 8;
    const WORDS: usize = 32 + 2 * D as usize;
    const PLACE: Place<2, D, WORDS> = Place::new::<0, 16, 32>();

    fn page() -> [AtomicU32; WORDS] {
        core::array::from_fn(|_| AtomicU32::new(0))
    }

    fn ends(page: &[AtomicU32; WORDS]) -> (Producer<2, D, WORDS>, Consumer<2, D, WORDS>) {
        (Producer::new(page, PLACE), Consumer::new(page, PLACE))
    }

    fn plain(words: Option<[Untrusted<u32>; 2]>) -> Option<[u32; 2]> {
        words.map(|w| w.map(|w| w.at_most(u32::MAX.into()).unwrap() as u32))
    }

    #[test]
    fn an_entry_is_nobodys_before_it_is_published() {
        let page = page();
        let (mut tx, mut rx) = ends(&page);
        assert_eq!(tx.push(&page, [3, 4]), Ok(true));
        assert_eq!(rx.pop(&page).map(plain), Ok(None));
        assert!(tx.publish(&page));
        assert!(!tx.publish(&page), "nothing new, nothing published");
        assert_eq!(rx.pop(&page).map(plain), Ok(Some([3, 4])));
        assert_eq!(rx.pop(&page).map(plain), Ok(None));
    }

    /// The ring wraps many times over, and space is exactly what the consumer
    /// has given back.
    #[test]
    fn the_ring_wraps_and_counts_its_space() {
        let page = page();
        let (mut tx, mut rx) = ends(&page);
        let (mut pushed, mut popped) = (0u32, 0u32);
        for round in 0..5 * D {
            for _ in 0..1 + round % 9 {
                assert_eq!(tx.space(&page), Ok(D - (pushed - popped)));
                if !tx.push(&page, [pushed, !pushed]).unwrap() {
                    assert_eq!(pushed - popped, D);
                    break;
                }
                pushed += 1;
            }
            tx.publish(&page);
            for _ in 0..1 + round % 7 {
                let Some(words) = plain(rx.pop(&page).unwrap()) else { break };
                assert_eq!(words, [popped, !popped], "entries come out whole, in the order they went in");
                popped += 1;
            }
            rx.release(&page);
        }
        assert!(pushed > 2 * D, "the ring wrapped");
    }

    /// A tail more than the ring past what was released, and a head past what
    /// was published, each end the session by name; a cursor moved backwards
    /// within bounds costs only its owner.
    #[test]
    fn a_peer_cursor_out_of_reach_is_a_violation() {
        let page = page();
        let (mut tx, mut rx) = ends(&page);
        page[16].store(D + 1, Ordering::Release);
        assert_eq!(rx.pop(&page), Err(Violation::TailPastDepth));
        page[16].store(u32::MAX, Ordering::Release);
        assert_eq!(rx.pop(&page), Err(Violation::TailPastDepth), "a tail behind what was released");
        page[0].store(1, Ordering::Release);
        assert_eq!(tx.space(&page), Err(Violation::HeadPastTail));
        page[0].store(u32::MAX, Ordering::Release);
        assert_eq!(tx.space(&page), Ok(D - 1), "a head moved back costs its consumer the room");
    }

    /// A producer that steps its tail one past each entry popped, none of them
    /// released: the ring's depth is taken, and the next tail is past it.
    #[test]
    fn a_tail_stepped_past_each_pop_is_refused_at_the_depth() {
        let page = page();
        let (_, mut rx) = ends(&page);
        let mut taken = 0;
        let refused = loop {
            page[16].store(taken + 1, Ordering::Release);
            match rx.pop(&page) {
                Ok(Some(_)) => taken += 1,
                Ok(None) => panic!("a published entry was not taken"),
                Err(violation) => break violation,
            }
            assert!(taken <= D, "took {taken} entries from a ring of {D} without releasing one");
        };
        assert_eq!((taken, refused), (D, Violation::TailPastDepth));
    }

    /// A consumer that steps its head onto each entry pushed, none of them
    /// published: the ring's depth is pushed, and the next head is past what
    /// was published.
    #[test]
    fn a_head_stepped_past_each_push_is_refused_at_the_depth() {
        let page = page();
        let (mut tx, _) = ends(&page);
        let mut pushed = 0;
        let refused = loop {
            match tx.push(&page, [pushed, pushed]) {
                Ok(true) => pushed += 1,
                Ok(false) => panic!("a ring with nothing published was full"),
                Err(violation) => break violation,
            }
            page[0].store(pushed, Ordering::Release);
            assert!(pushed <= D, "pushed {pushed} into a ring of {D} with none published");
        };
        assert_eq!((pushed, refused), (D, Violation::HeadPastTail));
    }
}
