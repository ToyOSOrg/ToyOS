//! A single-producer queue of `E`-word entries, `D` deep.
//!
//! **A producer writes an entry's words, then publishes the tail with
//! `Release`; a consumer loads the tail with `Acquire`, then reads the words.**
//! The head goes back the other way. Each end looks at the peer's cursor when
//! what it last saw is spent, not once per entry, and stores its own once per
//! batch.

use core::sync::atomic::Ordering;

use crate::{word, Asleep, Cursors, Untrusted, Violation, Wake, Word};

/// Where one queue is in a region, in words: its cursors, and its first entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Place {
    pub cursors: Cursors,
    pub entries: usize,
}

impl Place {
    /// The words of the entry at ring position `at`.
    fn entry<'a, W, const E: usize, const D: u32>(&self, page: &'a [W], at: u32) -> Result<&'a [W], Violation> {
        let first = usize::try_from(at & D.wrapping_sub(1))
            .ok()
            .and_then(|slot| slot.checked_mul(E))
            .and_then(|offset| offset.checked_add(self.entries));
        first.and_then(|first| page.get(first..first.checked_add(E)?)).ok_or(Violation::Region)
    }

    /// Every word the queue uses is in `page`.
    fn check<W, const E: usize, const D: u32>(&self, page: &[W]) -> Result<(), Violation> {
        const { assert!(D.is_power_of_two() && E > 0, "a queue is a power of two deep, of entries of a word or more") };
        word(page, self.cursors.head)?;
        word(page, self.cursors.tail)?;
        word(page, self.cursors.sleep)?;
        self.entry::<W, E, D>(page, D.wrapping_sub(1)).map(|_| ())
    }
}

/// The end of a queue that writes entries. It holds its cursors and not the
/// region; every call is given the region's words.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Producer<const E: usize, const D: u32> {
    place: Place,
    local: u32,
    published: u32,
    /// Entries that may be pushed before the head is looked at again.
    room: u32,
}

impl<const E: usize, const D: u32> Producer<E, D> {
    /// This end of the queue at `place`, its tail stored 0.
    pub fn new<W: Word>(page: &[W], place: Place) -> Result<Self, Violation> {
        place.check::<W, E, D>(page)?;
        word(page, place.cursors.tail)?.store(0, Ordering::Release);
        Ok(Self { place, local: 0, published: 0, room: D })
    }

    /// How many entries may be pushed before the consumer frees more.
    pub fn space<W: Word>(&mut self, page: &[W]) -> Result<u32, Violation> {
        let unreleased = self.place.cursors.unreleased(page, self.published, D)?;
        let pending = self.local.wrapping_sub(self.published);
        self.room = D.saturating_sub(pending.saturating_add(unreleased));
        Ok(self.room)
    }

    /// Write one entry, or answer `false` for a full queue and write nothing.
    /// It is the consumer's once [`Self::publish`] runs.
    pub fn push<W: Word>(&mut self, page: &[W], words: [u32; E]) -> Result<bool, Violation> {
        if self.room == 0 && self.space(page)? == 0 {
            return Ok(false);
        }
        for (shared, value) in self.place.entry::<W, E, D>(page, self.local)?.iter().zip(words) {
            shared.store(value, Ordering::Relaxed);
        }
        self.local = self.local.wrapping_add(1);
        self.room = self.room.wrapping_sub(1);
        Ok(true)
    }

    /// Publish every entry pushed so far; `None` if there was none.
    pub fn publish<W: Word>(&mut self, page: &[W]) -> Result<Option<Wake>, Violation> {
        if self.published == self.local {
            return Ok(None);
        }
        let wake = self.place.cursors.publish(page, self.local)?;
        self.published = self.local;
        Ok(Some(wake))
    }
}

/// The end of a queue that reads entries; like [`Producer`], it holds cursors
/// and is given the region's words.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Consumer<const E: usize, const D: u32> {
    place: Place,
    local: u32,
    released: u32,
    /// Entries published and not yet popped, as last seen.
    ready: u32,
    /// This end stored its `sleep` word and has not cleared it.
    asleep: bool,
}

impl<const E: usize, const D: u32> Consumer<E, D> {
    /// This end of the queue at `place`, its head and `sleep` stored 0.
    pub fn new<W: Word>(page: &[W], place: Place) -> Result<Self, Violation> {
        place.check::<W, E, D>(page)?;
        word(page, place.cursors.head)?.store(0, Ordering::Release);
        place.cursors.awake(page)?;
        Ok(Self { place, local: 0, released: 0, ready: 0, asleep: false })
    }

    /// Look at the tail again, and answer it; what of it is not popped is
    /// `ready`.
    fn observe<W: Word>(&mut self, page: &[W]) -> Result<u32, Violation> {
        let published = self.place.cursors.published(page, self.released, D)?;
        self.ready = published.saturating_sub(self.local.wrapping_sub(self.released));
        Ok(self.released.wrapping_add(published))
    }

    /// The next published entry, or `None` for none.
    pub fn pop<W: Word>(&mut self, page: &[W]) -> Result<Option<[Untrusted<u32>; E]>, Violation> {
        if self.ready == 0 {
            if self.asleep {
                self.place.cursors.awake(page)?;
                self.asleep = false;
            }
            self.observe(page)?;
            if self.ready == 0 {
                return Ok(None);
            }
        }
        let mut words = [Untrusted::new(0); E];
        for (out, shared) in words.iter_mut().zip(self.place.entry::<W, E, D>(page, self.local)?) {
            *out = Untrusted::new(shared.load(Ordering::Relaxed));
        }
        self.local = self.local.wrapping_add(1);
        self.ready = self.ready.wrapping_sub(1);
        Ok(Some(words))
    }

    /// Give every entry popped so far back to the producer.
    pub fn release<W: Word>(&mut self, page: &[W]) -> Result<(), Violation> {
        if self.released != self.local {
            word(page, self.place.cursors.head)?.store(self.local, Ordering::Release);
            self.released = self.local;
        }
        Ok(())
    }

    /// Say this end sleeps, and look once more: `None` if an entry is there
    /// after all, or where to wait. A producer that publishes after this is
    /// answered [`Wake::Peer`]; the next [`Self::pop`] says this end is awake.
    pub fn before_sleep<W: Word>(&mut self, page: &[W]) -> Result<Option<Asleep>, Violation> {
        if self.ready > 0 {
            return Ok(None);
        }
        self.place.cursors.sleep(page)?;
        self.asleep = true;
        let tail = self.observe(page)?;
        if self.ready > 0 {
            self.place.cursors.awake(page)?;
            self.asleep = false;
            return Ok(None);
        }
        Ok(Some(Asleep { word: self.place.cursors.tail, value: tail }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::sync::atomic::AtomicU32;

    const D: u32 = 8;
    const PLACE: Place = Place { cursors: Cursors { head: 0, tail: 16, sleep: 1 }, entries: 32 };

    fn page() -> Vec<AtomicU32> {
        (0..32 + 2 * D as usize).map(|_| AtomicU32::new(0)).collect()
    }

    fn ends(page: &[AtomicU32]) -> (Producer<2, D>, Consumer<2, D>) {
        (Producer::new(page, PLACE).unwrap(), Consumer::new(page, PLACE).unwrap())
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
        assert_eq!(tx.publish(&page), Ok(Some(Wake::Busy)));
        assert_eq!(tx.publish(&page), Ok(None), "nothing new, nothing published");
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
            let _ = tx.publish(&page).unwrap();
            for _ in 0..1 + round % 7 {
                let Some(words) = plain(rx.pop(&page).unwrap()) else { break };
                assert_eq!(words, [popped, !popped], "entries come out whole, in the order they went in");
                popped += 1;
            }
            rx.release(&page).unwrap();
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

    #[test]
    fn a_place_outside_the_words_is_refused() {
        let page = page();
        let short = &page[..32 + 2 * D as usize - 1];
        assert_eq!(Producer::<2, D>::new(short, PLACE).err(), Some(Violation::Region));
        assert_eq!(Consumer::<2, D>::new(short, PLACE).err(), Some(Violation::Region));
    }

    /// The wake's two halves on one thread: a consumer that said it sleeps is
    /// woken by the next publish and by no later one once it has popped.
    #[test]
    fn a_sleeper_is_woken_once_and_a_busy_one_never() {
        let page = page();
        let (mut tx, mut rx) = ends(&page);
        assert_eq!(rx.before_sleep(&page), Ok(Some(Asleep { word: 16, value: 0 })));
        tx.push(&page, [1, 1]).unwrap();
        assert_eq!(tx.publish(&page), Ok(Some(Wake::Peer)));
        assert!(rx.pop(&page).unwrap().is_some());
        assert_eq!(rx.pop(&page), Ok(None), "the pop that found nothing said this end is awake");
        tx.push(&page, [2, 2]).unwrap();
        assert_eq!(tx.publish(&page), Ok(Some(Wake::Busy)));
        assert_eq!(rx.before_sleep(&page), Ok(None), "an entry was there after all");
        assert_eq!(tx.publish(&page), Ok(None));
    }
}
