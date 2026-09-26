//! The two single-producer rings on a session's page.
//!
//! **A producer writes an entry's words, then publishes the tail with
//! `Release`; a consumer loads the tail with `Acquire`, then reads the words.**
//! The head goes back the other way: a consumer publishes it with `Release`
//! only after the entries below it are read, and a producer loads it with
//! `Acquire` before it writes over them. `tests/loom_ring.rs` holds the
//! first edge; the second is the same pair turned round.
//!
//! Each end keeps its own index in a local and only ever *stores* the shared
//! one, so nothing the peer writes into its own index can move ours; what the
//! peer's index claims is bounded against ours before it is believed
//! ([`Violation`]).

use core::sync::atomic::{AtomicU32, Ordering};

use crate::layout::{CQE_WORDS, CQ_BASE, CQ_HEAD, CQ_TAIL, DEPTH, RING_WORDS, SQE_WORDS, SQ_BASE, SQ_HEAD, SQ_TAIL};

/// One shared 32-bit word: an atomic over the mapped page, or a model's.
pub trait Word {
    fn load(&self, order: Ordering) -> u32;
    fn store(&self, value: u32, order: Ordering);
}

impl Word for AtomicU32 {
    fn load(&self, order: Ordering) -> u32 {
        AtomicU32::load(self, order)
    }
    fn store(&self, value: u32, order: Ordering) {
        AtomicU32::store(self, value, order)
    }
}

/// The peer's index says something no peer of this protocol can: more entries
/// published than the ring holds, or more consumed than were produced. The
/// session is over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Violation;

/// The publishing order a tail is stored with.
#[cfg(not(feature = "mutate-ring-publish-relaxed"))]
const PUBLISH: Ordering = Ordering::Release;
#[cfg(feature = "mutate-ring-publish-relaxed")]
const PUBLISH: Ordering = Ordering::Relaxed;

/// Where one ring is on the page, in words.
#[derive(Clone, Copy, Debug)]
struct Place {
    head: usize,
    tail: usize,
    entries: usize,
}

const REQUESTS: Place = Place { head: SQ_HEAD, tail: SQ_TAIL, entries: SQ_BASE };
const COMPLETIONS: Place = Place { head: CQ_HEAD, tail: CQ_TAIL, entries: CQ_BASE };

fn checked<W>(page: &[W]) -> &[W] {
    assert!(page.len() >= RING_WORDS, "a session page holds every ring word");
    page
}

/// The end of a ring that writes entries. It holds its indices and not the
/// page, so its owner keeps the mapping beside it; every call is given the
/// page.
#[derive(Debug)]
pub struct Producer<const N: usize> {
    place: Place,
    local: u32,
    published: u32,
}

impl<const N: usize> Producer<N> {
    fn new<W: Word>(page: &[W], place: Place) -> Self {
        checked(page)[place.tail].store(0, Ordering::Release);
        Self { place, local: 0, published: 0 }
    }

    /// How many entries may be pushed before the consumer frees more.
    pub fn space<W: Word>(&self, page: &[W]) -> Result<u32, Violation> {
        let head = checked(page)[self.place.head].load(Ordering::Acquire);
        let used = self.local.wrapping_sub(head);
        if used > DEPTH {
            return Err(Violation);
        }
        Ok(DEPTH - used)
    }

    /// Write one entry. It is the consumer's only once [`Self::publish`] runs.
    ///
    /// # Panics
    /// When the ring has no space: the caller asks [`Self::space`] first.
    pub fn push<W: Word>(&mut self, page: &[W], words: [u32; N]) {
        assert!(self.space(page).is_ok_and(|space| space > 0), "a push into a full ring");
        let at = self.place.entries + (self.local % DEPTH) as usize * N;
        for (i, word) in words.into_iter().enumerate() {
            page[at + i].store(word, Ordering::Relaxed);
        }
        self.local = self.local.wrapping_add(1);
    }

    /// Publish every entry pushed so far; answers whether there was any.
    pub fn publish<W: Word>(&mut self, page: &[W]) -> bool {
        if self.published == self.local {
            return false;
        }
        checked(page)[self.place.tail].store(self.local, PUBLISH);
        self.published = self.local;
        true
    }
}

/// The end of a ring that reads entries; like [`Producer`], it holds indices
/// and is given the page.
#[derive(Debug)]
pub struct Consumer<const N: usize> {
    place: Place,
    local: u32,
    released: u32,
}

impl<const N: usize> Consumer<N> {
    fn new<W: Word>(page: &[W], place: Place) -> Self {
        checked(page)[place.head].store(0, Ordering::Release);
        Self { place, local: 0, released: 0 }
    }

    /// The next published entry, `None` for none, or the producer's tail
    /// claiming more than the ring holds.
    pub fn pop<W: Word>(&mut self, page: &[W]) -> Result<Option<[u32; N]>, Violation> {
        let tail = checked(page)[self.place.tail].load(Ordering::Acquire);
        let ready = tail.wrapping_sub(self.local);
        if ready > DEPTH {
            return Err(Violation);
        }
        if ready == 0 {
            return Ok(None);
        }
        let at = self.place.entries + (self.local % DEPTH) as usize * N;
        let words = core::array::from_fn(|i| page[at + i].load(Ordering::Relaxed));
        self.local = self.local.wrapping_add(1);
        Ok(Some(words))
    }

    /// Give every entry popped so far back to the producer.
    pub fn release<W: Word>(&mut self, page: &[W]) {
        if self.released != self.local {
            checked(page)[self.place.head].store(self.local, Ordering::Release);
            self.released = self.local;
        }
    }
}

/// A client's two ends: requests out, completions in.
pub type ClientRings = (Producer<SQE_WORDS>, Consumer<CQE_WORDS>);

/// A server's two ends: requests in, completions out.
pub type ServerRings = (Consumer<SQE_WORDS>, Producer<CQE_WORDS>);

/// The client's ends of a session page, both indices it owns set to 0. Done
/// before the page is sent to a server, and again before it is sent to the
/// next one.
pub fn client<W: Word>(page: &[W]) -> ClientRings {
    (Producer::new(page, REQUESTS), Consumer::new(page, COMPLETIONS))
}

/// The server's ends of a session page it was sent, both indices it owns set
/// to 0. Whatever the client left in its own two is bounded by the first
/// [`Consumer::pop`] and [`Producer::space`].
pub fn server<W: Word>(page: &[W]) -> ServerRings {
    (Consumer::new(page, REQUESTS), Producer::new(page, COMPLETIONS))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{Completion, Op, Request, Status};
    use alloc::vec::Vec;

    fn page() -> Vec<AtomicU32> {
        (0..RING_WORDS).map(|_| AtomicU32::new(0)).collect()
    }

    #[test]
    fn a_request_crosses_and_its_completion_comes_back() {
        let page = page();
        let (mut sq, mut cq) = client(&page);
        let (mut rq, mut cp) = server(&page);
        let request = Request { op: Op::Write, tag: 3, lba: 8, blocks: 2, arena: 1 };
        sq.push(&page, request.encode());
        assert_eq!(rq.pop(&page), Ok(None), "an entry is nobody's before it is published");
        assert!(sq.publish(&page));
        assert_eq!(rq.pop(&page).map(|w| w.map(|w| Request::decode(w, 100))), Ok(Some(Ok(request))));
        rq.release(&page);
        cp.push(&page, Completion { tag: 3, status: Status::Ok }.encode());
        cp.publish(&page);
        assert_eq!(cq.pop(&page).map(|w| w.and_then(Completion::decode)), Ok(Some(Completion { tag: 3, status: Status::Ok })));
    }

    /// The ring wraps many times over, and space is exactly what the consumer
    /// has given back.
    #[test]
    fn the_ring_wraps_and_counts_its_space() {
        let page = page();
        let (mut sq, _) = client(&page);
        let (mut rq, _) = server(&page);
        let (mut pushed, mut popped) = (0u32, 0u32);
        for round in 0..5 * DEPTH {
            // Uneven batches both ways, so the two indices cross every slot
            // at every distance.
            for _ in 0..1 + round % 9 {
                assert_eq!(sq.space(&page), Ok(DEPTH - (pushed - popped)));
                if pushed - popped == DEPTH {
                    break;
                }
                sq.push(&page, Request { op: Op::Read, tag: pushed, lba: 0, blocks: 1, arena: 0 }.encode());
                pushed += 1;
            }
            sq.publish(&page);
            for _ in 0..1 + round % 7 {
                let Ok(Some(words)) = rq.pop(&page) else { break };
                assert_eq!(words[1], popped, "entries come out in the order they went in");
                popped += 1;
            }
            rq.release(&page);
        }
        assert!(pushed > 2 * DEPTH, "the ring wrapped");
    }

    /// A hostile producer's tail far ahead of the consumer, and a hostile
    /// consumer's head ahead of what was produced, both end the session.
    #[test]
    fn a_peer_index_out_of_reach_is_a_violation() {
        let page = page();
        let (sq, _) = client(&page);
        let (mut rq, _) = server(&page);
        page[SQ_TAIL].store(DEPTH + 1, Ordering::Release);
        assert_eq!(rq.pop(&page), Err(Violation));
        page[SQ_HEAD].store(5, Ordering::Release);
        assert_eq!(sq.space(&page), Err(Violation));
    }
}
