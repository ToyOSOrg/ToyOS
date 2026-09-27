//! A single-producer byte ring with free-running cursors, and the producer's
//! `end` word.
//!
//! **This crate never touches a byte.** Each end answers the [`Span`]s of the
//! region the adapter copies into or out of, once, and the cursors publish
//! them as a queue's do. The ring's capacity is a power of two, so a cursor
//! wrapping at 2³² lands on the same byte.
//!
//! **The end is stored after the tail and loaded before it**, so a reader that
//! sees [`End::Fin`] and no bytes has seen the stream's last byte.

use core::sync::atomic::Ordering;

use crate::{word, Asleep, Cursors, Span, Untrusted, Violation, Wake, Word};

/// Where one stream is: its cursors, its `end` word, and its bytes — a fixed
/// span of the region or an arena run's ([`crate::Run::span`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StreamPlace {
    pub cursors: Cursors,
    pub end: usize,
    pub data: Span,
}

impl StreamPlace {
    /// The capacity, once every word is in `page` and the bytes are a power of
    /// two.
    fn check<W>(&self, page: &[W]) -> Result<u32, Violation> {
        for at in [self.cursors.head, self.cursors.tail, self.cursors.sleep, self.end] {
            word(page, at)?;
        }
        u32::try_from(self.data.len).ok().filter(|cap| cap.is_power_of_two()).ok_or(Violation::Region)
    }

    /// The bytes from cursor `at`, `len` long, where they are: past the ring's
    /// end they go on from its start.
    fn spans(&self, cap: u32, at: u32, len: u32) -> Result<[Span; 2], Violation> {
        let from = at & cap.wrapping_sub(1);
        let first = len.min(cap.wrapping_sub(from));
        let bytes = |n: u32| usize::try_from(n).map_err(|_| Violation::Region);
        let offset = self.data.offset.checked_add(bytes(from)?).ok_or(Violation::Region)?;
        Ok([
            Span { offset, len: bytes(first)? },
            Span { offset: self.data.offset, len: bytes(len.wrapping_sub(first))? },
        ])
    }
}

/// What the producer has said of the bytes after the last it published.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum End {
    /// More may follow.
    Open,
    /// None follow: the stream ended whole.
    Fin,
    /// None follow, and what was not read is abandoned.
    Reset,
}

impl End {
    const fn word(self) -> u32 {
        match self {
            Self::Open => 0,
            Self::Fin => 1,
            Self::Reset => 2,
        }
    }

    fn decode(word: Untrusted<u32>) -> Result<Self, Violation> {
        [Self::Open, Self::Fin, Self::Reset].into_iter().find(|end| word.is(end.word())).ok_or(Violation::End)
    }
}

/// The end of a stream that writes bytes.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StreamTx {
    place: StreamPlace,
    cap: u32,
    local: u32,
    published: u32,
}

impl StreamTx {
    /// This end of the stream at `place`, its tail stored 0 and its end open.
    pub fn new<W: Word>(page: &[W], place: StreamPlace) -> Result<Self, Violation> {
        let cap = place.check(page)?;
        word(page, place.cursors.tail)?.store(0, Ordering::Release);
        word(page, place.end)?.store(End::Open.word(), Ordering::Release);
        Ok(Self { place, cap, local: 0, published: 0 })
    }

    /// Room for at most `want` bytes, taken now: the caller fills the spans,
    /// then publishes.
    pub fn write<W: Word>(&mut self, page: &[W], want: u32) -> Result<[Span; 2], Violation> {
        let unreleased = self.place.cursors.unreleased(page, self.published, self.cap)?;
        let pending = self.local.wrapping_sub(self.published);
        let len = want.min(self.cap.saturating_sub(pending.saturating_add(unreleased)));
        let spans = self.place.spans(self.cap, self.local, len)?;
        self.local = self.local.wrapping_add(len);
        Ok(spans)
    }

    /// Publish every byte written so far; `None` if there was none.
    pub fn publish<W: Word>(&mut self, page: &[W]) -> Result<Option<Wake>, Violation> {
        if self.published == self.local {
            return Ok(None);
        }
        let wake = self.place.cursors.publish(page, self.local)?;
        self.published = self.local;
        Ok(Some(wake))
    }

    /// Publish what was written and say what follows it.
    pub fn close<W: Word>(&mut self, page: &[W], end: End) -> Result<Wake, Violation> {
        word(page, self.place.cursors.tail)?.store(self.local, Ordering::Release);
        self.published = self.local;
        word(page, self.place.end)?.store(end.word(), Ordering::Release);
        self.place.cursors.wake(page)
    }
}

/// The end of a stream that reads bytes.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StreamRx {
    place: StreamPlace,
    cap: u32,
    local: u32,
    released: u32,
    asleep: bool,
}

impl StreamRx {
    /// This end of the stream at `place`, its head and `sleep` stored 0.
    pub fn new<W: Word>(page: &[W], place: StreamPlace) -> Result<Self, Violation> {
        let cap = place.check(page)?;
        word(page, place.cursors.head)?.store(0, Ordering::Release);
        place.cursors.awake(page)?;
        Ok(Self { place, cap, local: 0, released: 0, asleep: false })
    }

    /// What was said of the end, then how many bytes are ready past what was
    /// read, and the tail they end at.
    fn observe<W: Word>(&self, page: &[W]) -> Result<(End, u32, u32), Violation> {
        let end = End::decode(Untrusted::new(word(page, self.place.end)?.load(Ordering::Acquire)))?;
        let published = self.place.cursors.published(page, self.released, self.cap)?;
        let ready = published.saturating_sub(self.local.wrapping_sub(self.released));
        Ok((end, ready, self.released.wrapping_add(published)))
    }

    /// At most `want` bytes, taken now, and what follows them: the caller
    /// copies the spans out, then releases.
    pub fn read<W: Word>(&mut self, page: &[W], want: u32) -> Result<([Span; 2], End), Violation> {
        if self.asleep {
            self.place.cursors.awake(page)?;
            self.asleep = false;
        }
        let (end, ready, _) = self.observe(page)?;
        let len = want.min(ready);
        let spans = self.place.spans(self.cap, self.local, len)?;
        self.local = self.local.wrapping_add(len);
        Ok((spans, end))
    }

    /// Give every byte read so far back to the producer.
    pub fn release<W: Word>(&mut self, page: &[W]) -> Result<(), Violation> {
        if self.released != self.local {
            word(page, self.place.cursors.head)?.store(self.local, Ordering::Release);
            self.released = self.local;
        }
        Ok(())
    }

    /// As [`crate::Consumer::before_sleep`]; an end said is something to read.
    pub fn before_sleep<W: Word>(&mut self, page: &[W]) -> Result<Option<Asleep>, Violation> {
        self.place.cursors.sleep(page)?;
        self.asleep = true;
        let (end, ready, tail) = self.observe(page)?;
        if ready > 0 || end != End::Open {
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

    const PLACE: StreamPlace = StreamPlace {
        cursors: Cursors { head: 0, tail: 16, sleep: 1 },
        end: 17,
        data: Span { offset: 4096, len: 16 },
    };

    fn page() -> Vec<AtomicU32> {
        (0..32).map(|_| AtomicU32::new(0)).collect()
    }

    /// The bytes behind the spans, as the adapter holds them.
    fn copy(ring: &mut [u8; 16], spans: [Span; 2], bytes: &[u8], into: bool) -> Vec<u8> {
        let mut out = Vec::new();
        let mut at = 0;
        for span in spans {
            let ring = &mut ring[span.offset - 4096..span.offset - 4096 + span.len];
            if into {
                ring.copy_from_slice(&bytes[at..at + span.len]);
            }
            out.extend_from_slice(ring);
            at += span.len;
        }
        out
    }

    /// Bytes cross whole and in order as the cursors wrap the ring, a write is
    /// never given more room than was released, and `Fin` arrives after the
    /// last byte.
    #[test]
    fn bytes_cross_whole_around_the_ring_and_end_after_the_last() {
        let page = page();
        let mut ring = [0u8; 16];
        let mut tx = StreamTx::new(&page, PLACE).unwrap();
        let mut rx = StreamRx::new(&page, PLACE).unwrap();
        let (mut sent, mut got) = (Vec::new(), Vec::new());
        for n in 0..40u8 {
            let chunk: Vec<u8> = (0..n % 11).map(|i| n.wrapping_mul(31).wrapping_add(i)).collect();
            let spans = tx.write(&page, chunk.len() as u32).unwrap();
            let len: usize = spans.iter().map(|s| s.len).sum();
            assert!(len <= 16 - (sent.len() - got.len()), "room past what was released");
            sent.extend_from_slice(&copy(&mut ring, spans, &chunk[..len], true));
            let _ = tx.publish(&page).unwrap();
            let (spans, end) = rx.read(&page, u32::from(n % 5)).unwrap();
            assert_eq!(end, End::Open);
            got.extend(copy(&mut ring, spans, &[], false));
            rx.release(&page).unwrap();
        }
        assert_eq!(tx.close(&page, End::Fin), Ok(Wake::Busy));
        loop {
            let (spans, end) = rx.read(&page, 16).unwrap();
            let chunk = copy(&mut ring, spans, &[], false);
            got.extend_from_slice(&chunk);
            rx.release(&page).unwrap();
            if chunk.is_empty() {
                assert_eq!(end, End::Fin);
                break;
            }
        }
        assert_eq!(got, sent);
        assert!(sent.len() > 64, "the ring wrapped");
    }

    #[test]
    fn an_end_word_no_producer_writes_is_a_violation() {
        let page = page();
        let mut rx = StreamRx::new(&page, PLACE).unwrap();
        page[17].store(3, Ordering::Release);
        assert_eq!(rx.read(&page, 1), Err(Violation::End));
        page[17].store(0, Ordering::Release);
        page[16].store(17, Ordering::Release);
        assert_eq!(rx.read(&page, 1), Err(Violation::TailPastDepth));
    }

    #[test]
    fn a_ring_that_is_not_a_power_of_two_is_refused() {
        let page = page();
        let place = StreamPlace { data: Span { offset: 4096, len: 24 }, ..PLACE };
        assert_eq!(StreamTx::new(&page, place).err(), Some(Violation::Region));
    }

    /// A reader that said it sleeps is woken by the close, and one that looks
    /// after a close does not sleep.
    #[test]
    fn a_close_wakes_a_sleeper_and_keeps_the_next_awake() {
        let page = page();
        let mut tx = StreamTx::new(&page, PLACE).unwrap();
        let mut rx = StreamRx::new(&page, PLACE).unwrap();
        assert_eq!(rx.before_sleep(&page), Ok(Some(Asleep { word: 16, value: 0 })));
        assert_eq!(tx.close(&page, End::Reset), Ok(Wake::Peer));
        assert_eq!(rx.before_sleep(&page), Ok(None));
    }
}
