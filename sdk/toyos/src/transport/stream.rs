//! A single-producer ring of `C` bytes, its cursors and wants among `N` words.
//!
//! **A producer copies bytes in through the [`Span`]s it is handed, then
//! publishes its tail with `Release`; a consumer loads the tail with `Acquire`,
//! then copies the bytes out.** The head goes back the other way. Either end
//! addresses bytes by their offset from its own cursor, so a producer may place
//! a byte anywhere in its room before it advances over it. An end reads the
//! peer's cursor only when it asks for its room or what is ready, and stores
//! its own once a batch.
//!
//! **A cursor is a position modulo twice the ring**, so a full ring and an empty
//! one differ and the ring may be any size. The peer's is believed only as a
//! position at most the ring away from what this end last stored; otherwise
//! [`Violation::TailPastDepth`] or [`Violation::HeadPastTail`] names it. Every
//! span lies in the ring whatever the peer wrote.
//!
//! **A sleeper never misses a wake.** An end about to sleep [`arm`]s its want,
//! which exchanges 1 into it and only then looks at the peer's cursor again; it
//! sleeps only if that look finds nothing. An end that publishes or releases
//! exchanges the peer's want for 0 after its cursor and rings the doorbell only
//! if the want was set. The two exchanges are on one word, so the waker's is
//! read by the sleeper's and carries its cursor with it, or follows it and
//! finds it set. A sleeper drains its doorbell before it looks, never between
//! arming and sleeping, or it can drain the ring that answers its want.
//!
//! **The peer's want words cost at most a doorbell**: any value but 0 is set.
//!
//! **An end may have several holders**, which take turns under a gate of the
//! session's. Such a holder keeps no cursor between turns: it [`resume`]s its
//! own from the word the last holder stored, and refuses one past the lap.
//!
//! [`arm`]: Consumer::arm
//! [`resume`]: Consumer::resume

use core::sync::atomic::Ordering;

use super::{clamp, word, Span, Violation, Word, PUBLISH};

/// A sleeper's exchange: what the waker published before clearing the want is
/// seen by the look that follows.
const ARM: Ordering = Ordering::Acquire;
#[cfg(not(feature = "transport-want-relaxed"))]
pub(super) const WAKE: Ordering = Ordering::Release;
#[cfg(feature = "transport-want-relaxed")]
pub(super) const WAKE: Ordering = Ordering::Relaxed;
/// The head's edge: a producer that loads the head copies into a byte only
/// after the consumer's copy out of it, which the consumer's store carries.
const GIVE_BACK: Ordering = if cfg!(feature = "transport-head-relaxed") { Ordering::Relaxed } else { Ordering::Release };
const TAKE_BACK: Ordering = if cfg!(feature = "transport-head-relaxed") { Ordering::Relaxed } else { Ordering::Acquire };

/// `n` as an offset into the region; `Place::new` holds `usize` to every `u32`.
#[allow(clippy::as_conversions)]
const fn bytes(n: u32) -> usize {
    n as usize
}

/// Where a ring of `C` bytes is: its four words among `N` and the region offset
/// of its first byte.
///
/// ```
/// use toyos::transport::stream::Place;
/// const RING: Place<4096, 64> = Place::new::<0, 16, 32, 48, 4096>();
/// ```
///
/// A place with a word at `N` or past it does not compile:
///
/// ```compile_fail,E0080
/// use toyos::transport::stream::Place;
/// const RING: Place<4096, 64> = Place::new::<0, 16, 32, 64, 4096>();
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Place<const C: u32, const N: usize> {
    head: usize,
    tail: usize,
    /// Set by a consumer about to sleep on an empty ring.
    data: usize,
    /// Set by a producer about to sleep on a full ring.
    room: usize,
    base: usize,
}

impl<const C: u32, const N: usize> Place<C, N> {
    /// What a cursor counts modulo.
    const LAP: u32 = C.wrapping_mul(2);

    /// The cursor at word `at`, as an end's own last stored it.
    fn own<W: Word>(page: &[W; N], at: usize) -> Result<u32, Violation> {
        let cursor = word(page, at).load(Ordering::Acquire);
        if cursor < Self::LAP { Ok(cursor) } else { Err(Violation::OwnCursor) }
    }

    pub const fn new<const HEAD: usize, const TAIL: usize, const DATA: usize, const ROOM: usize, const BASE: usize>(
    ) -> Self {
        const {
            assert!(C > 0 && C <= 1 << 30, "a ring holds a byte and at most a gibibyte");
            assert!(usize::BITS >= u32::BITS, "a ring offset is a u32");
            assert!(HEAD < N && TAIL < N && DATA < N && ROOM < N, "a place names a word past its words");
            assert!(
                HEAD != TAIL && HEAD != DATA && HEAD != ROOM && TAIL != DATA && TAIL != ROOM && DATA != ROOM,
                "a ring's four words are four"
            );
            assert!(BASE.checked_add(bytes(C)).is_some(), "a ring ends inside the address space");
        }
        Self { head: HEAD, tail: TAIL, data: DATA, room: ROOM, base: BASE }
    }

    /// Its four words: head, tail and the two wants.
    pub(super) const fn words(&self) -> [usize; 4] {
        [self.head, self.tail, self.data, self.room]
    }

    /// `at` moved `n` on; both are below the lap.
    fn forward(at: u32, n: u32) -> u32 {
        let to = at.wrapping_add(n);
        if to >= Self::LAP { to.wrapping_sub(Self::LAP) } else { to }
    }

    /// How far `to` is past `from`; both are below the lap.
    fn distance(from: u32, to: u32) -> u32 {
        let apart = to.wrapping_sub(from);
        if to >= from { apart } else { apart.wrapping_add(Self::LAP) }
    }

    /// How far the peer's `claimed` position is from this end's, by `apart`:
    /// at most the ring. A word past the lap names no position, so it is
    /// farther than any ring.
    fn reach(claimed: u32, apart: impl FnOnce(u32) -> u32, broken: Violation) -> Result<u32, Violation> {
        clamp(if claimed < Self::LAP { apart(claimed) } else { u32::MAX }, C, broken)
    }

    /// The region's bytes `len` on from position `at`, in ring order: the
    /// second span is empty unless they wrap.
    fn spans(&self, at: u32, len: u32) -> [Span; 2] {
        let slot = if at >= C { at.wrapping_sub(C) } else { at };
        let first = len.min(C.wrapping_sub(slot));
        let start = self.base.wrapping_add(bytes(slot));
        [Span { offset: start, len: bytes(first) }, Span { offset: self.base, len: bytes(len.wrapping_sub(first)) }]
    }
}

/// The end of a ring that writes bytes. It holds its cursors and not the
/// region; every call is given the region's words.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Producer<const C: u32, const N: usize> {
    place: Place<C, N>,
    local: u32,
    published: u32,
    /// Bytes past `local` that may be written, as last seen.
    room: u32,
}

impl<const C: u32, const N: usize> Producer<C, N> {
    /// This end of the ring at `place`, its tail stored 0.
    pub fn new<W: Word>(page: &[W; N], place: Place<C, N>) -> Self {
        word(page, place.tail).store(0, Ordering::Release);
        Self { place, local: 0, published: 0, room: C }
    }

    /// The end at `place` as its holders left it, its tail resumed before each
    /// use ([`Self::resume`]).
    pub const fn joining(place: Place<C, N>) -> Self {
        Self { place, local: 0, published: 0, room: 0 }
    }

    /// Takes up the tail the end's last holder stored, nothing advanced past it.
    pub fn resume<W: Word>(&mut self, page: &[W; N]) -> Result<(), Violation> {
        if cfg!(feature = "transport-no-resume") {
            return Ok(());
        }
        let tail = Place::<C, N>::own(page, self.place.tail)?;
        *self = Self { local: tail, published: tail, room: 0, ..*self };
        Ok(())
    }

    /// How many bytes past the tail may be written, the consumer's head read
    /// again.
    pub fn room<W: Word>(&mut self, page: &[W; N]) -> Result<u32, Violation> {
        let head = word(page, self.place.head).load(TAKE_BACK);
        let published = self.published;
        let unreleased =
            Place::<C, N>::reach(head, |head| Place::<C, N>::distance(head, published), Violation::HeadPastTail)?;
        let pending = Place::<C, N>::distance(self.published, self.local);
        self.room = C.saturating_sub(unreleased.saturating_add(pending));
        Ok(self.room)
    }

    /// Where to copy the `len` bytes `at` past the tail; `None` past the room
    /// last seen.
    pub fn span(&self, at: u32, len: u32) -> Option<[Span; 2]> {
        (at.checked_add(len)? <= self.room).then(|| self.place.spans(Place::<C, N>::forward(self.local, at), len))
    }

    /// Count `n` more bytes written at the tail; `false`, and nothing counted,
    /// past the room last seen. They are the consumer's once
    /// [`Self::publish`] runs.
    #[must_use]
    pub fn advance(&mut self, n: u32) -> bool {
        let Some(room) = self.room.checked_sub(n) else { return false };
        self.local = Place::<C, N>::forward(self.local, n);
        self.room = room;
        true
    }

    /// Publish every byte advanced over so far; `true` if the consumer armed
    /// its want and the doorbell is to be rung.
    #[must_use]
    pub fn publish<W: Word>(&mut self, page: &[W; N]) -> bool {
        if self.published == self.local {
            return false;
        }
        word(page, self.place.tail).store(self.local, PUBLISH);
        self.published = self.local;
        word(page, self.place.data).swap(0, WAKE) != 0
    }

    /// Ask to be rung once the consumer releases, then read its head again:
    /// the room, and 0 is safe to sleep on.
    pub fn arm<W: Word>(&mut self, page: &[W; N]) -> Result<u32, Violation> {
        word(page, self.place.room).swap(1, ARM);
        self.room(page)
    }

    /// The consumer asked to be rung back: `true`, and its want exchanged for
    /// 0, if it is set and the consumer has bytes past its head or the ring
    /// `ended`. With neither, nothing is exchanged and the want waits for the
    /// next publish.
    pub fn ring_back<W: Word>(&self, page: &[W; N], ended: bool) -> Result<bool, Violation> {
        let head = word(page, self.place.head).load(TAKE_BACK);
        let published = self.published;
        let held =
            Place::<C, N>::reach(head, |head| Place::<C, N>::distance(head, published), Violation::HeadPastTail)?;
        Ok((held > 0 || ended) && word(page, self.place.data).swap(0, WAKE) != 0)
    }
}

/// The end of a ring that reads bytes; like [`Producer`], it holds cursors and
/// is given the region's words.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Consumer<const C: u32, const N: usize> {
    place: Place<C, N>,
    local: u32,
    released: u32,
    /// Bytes published past `local`, as last seen.
    ready: u32,
}

impl<const C: u32, const N: usize> Consumer<C, N> {
    /// This end of the ring at `place`, its head stored 0.
    pub fn new<W: Word>(page: &[W; N], place: Place<C, N>) -> Self {
        word(page, place.head).store(0, Ordering::Release);
        Self { place, local: 0, released: 0, ready: 0 }
    }

    /// The end at `place` as its holders left it, its head resumed before each
    /// use ([`Self::resume`]).
    pub const fn joining(place: Place<C, N>) -> Self {
        Self { place, local: 0, released: 0, ready: 0 }
    }

    /// Takes up the head the end's last holder stored, nothing advanced past it.
    pub fn resume<W: Word>(&mut self, page: &[W; N]) -> Result<(), Violation> {
        if cfg!(feature = "transport-no-resume") {
            return Ok(());
        }
        let head = Place::<C, N>::own(page, self.place.head)?;
        *self = Self { local: head, released: head, ready: 0, ..*self };
        Ok(())
    }

    /// How many bytes past the head are published, the producer's tail read
    /// again.
    pub fn ready<W: Word>(&mut self, page: &[W; N]) -> Result<u32, Violation> {
        let tail = word(page, self.place.tail).load(Ordering::Acquire);
        let released = self.released;
        let published =
            Place::<C, N>::reach(tail, |tail| Place::<C, N>::distance(released, tail), Violation::TailPastDepth)?;
        self.ready = published.saturating_sub(Place::<C, N>::distance(self.released, self.local));
        Ok(self.ready)
    }

    /// Where to copy the `len` bytes `at` past the head from; `None` past what
    /// was last seen ready.
    pub fn span(&self, at: u32, len: u32) -> Option<[Span; 2]> {
        (at.checked_add(len)? <= self.ready).then(|| self.place.spans(Place::<C, N>::forward(self.local, at), len))
    }

    /// Count `n` more bytes read at the head; `false`, and nothing counted,
    /// past what was last seen ready. They are the producer's once
    /// [`Self::release`] runs.
    #[must_use]
    pub fn advance(&mut self, n: u32) -> bool {
        let Some(ready) = self.ready.checked_sub(n) else { return false };
        self.local = Place::<C, N>::forward(self.local, n);
        self.ready = ready;
        true
    }

    /// Give every byte advanced over so far back to the producer; `true` if it
    /// armed its want and the doorbell is to be rung.
    #[must_use]
    pub fn release<W: Word>(&mut self, page: &[W; N]) -> bool {
        if self.released == self.local {
            return false;
        }
        word(page, self.place.head).store(self.local, GIVE_BACK);
        self.released = self.local;
        word(page, self.place.room).swap(0, WAKE) != 0
    }

    /// Ask to be rung once the producer publishes, then read its tail again:
    /// what is ready, and 0 is safe to sleep on.
    pub fn arm<W: Word>(&mut self, page: &[W; N]) -> Result<u32, Violation> {
        word(page, self.place.data).swap(1, ARM);
        self.ready(page)
    }

    /// The producer asked to be rung back: `true`, and its want exchanged for
    /// 0, if it is set and the producer has room past what this end released,
    /// or its direction `answers` without room. With neither, nothing is
    /// exchanged and the want waits for the next release.
    pub fn ring_back<W: Word>(&self, page: &[W; N], answers: bool) -> Result<bool, Violation> {
        let tail = word(page, self.place.tail).load(Ordering::Acquire);
        let released = self.released;
        let unreleased =
            Place::<C, N>::reach(tail, |tail| Place::<C, N>::distance(released, tail), Violation::TailPastDepth)?;
        Ok((unreleased < C || answers) && word(page, self.place.room).swap(0, WAKE) != 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::sync::atomic::AtomicU32;

    const WORDS: usize = 4;
    /// Off any alignment, so a span that forgets the base lands outside it.
    const BASE: usize = 3;
    /// Bytes on either side of the ring that no span may reach.
    const GUARD: usize = 3;

    fn page() -> [AtomicU32; WORDS] {
        core::array::from_fn(|_| AtomicU32::new(0))
    }

    fn place<const C: u32>() -> Place<C, WORDS> {
        Place::new::<0, 1, 2, 3, BASE>()
    }

    /// A region with the ring between two guards.
    fn region<const C: u32>() -> Vec<u8> {
        vec![0xA5; BASE + C as usize + GUARD]
    }

    /// The byte at stream position `at`: one read from the wrong slot or the
    /// wrong lap is, but for one chance in 256 a byte, another byte.
    fn byte(at: u64) -> u8 {
        (at.wrapping_add(1).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 56) as u8
    }

    /// xorshift64: deterministic, and seeded per run.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, n: u32) -> u32 {
            (self.next() % u64::from(n)) as u32
        }
    }

    fn inside<const C: u32>(spans: [Span; 2], len: u32) {
        assert_eq!(spans[0].len + spans[1].len, len as usize, "{spans:?} is not {len} bytes");
        for span in spans {
            assert!(span.offset >= BASE && span.offset + span.len <= BASE + C as usize, "{span:?} leaves the ring");
        }
    }

    fn put(region: &mut [u8], spans: [Span; 2], from: u64) {
        let mut at = from;
        for span in spans {
            for b in &mut region[span.offset..span.offset + span.len] {
                *b = byte(at);
                at += 1;
            }
        }
    }

    fn get(region: &[u8], spans: [Span; 2]) -> Vec<u8> {
        spans.iter().flat_map(|span| region[span.offset..span.offset + span.len].iter().copied()).collect()
    }

    fn expected(from: u64, len: u32) -> Vec<u8> {
        (from..from + u64::from(len)).map(byte).collect()
    }

    /// The oracle is a stream that never wraps: four counts of bytes since the
    /// start. The producer places each batch out of order, the consumer reads
    /// at offsets as well as at its head, and every room, every count ready and
    /// every byte read is what the counts say, round the lap many times.
    fn agrees<const C: u32>(seed: u64) {
        let page = page();
        let (mut tx, mut rx) = (Producer::new(&page, place::<C>()), Consumer::new(&page, place::<C>()));
        let mut region = region::<C>();
        let (mut written, mut published, mut read, mut released) = (0u64, 0u64, 0u64, 0u64);
        let mut rng = Rng(seed);
        for _ in 0..400 * C + 400 {
            match rng.below(4) {
                0 => {
                    let room = tx.room(&page).unwrap();
                    assert_eq!(u64::from(room), u64::from(C) - (written - released), "the room");
                    let n = rng.below(room + 1);
                    let split = rng.below(n + 1);
                    for (at, len) in [(split, n - split), (0, split)] {
                        put(&mut region, tx.span(at, len).unwrap(), written + u64::from(at));
                    }
                    assert!(tx.advance(n));
                    written += u64::from(n);
                }
                1 => {
                    let _ = tx.publish(&page);
                    published = written;
                }
                2 => {
                    let ready = rx.ready(&page).unwrap();
                    assert_eq!(u64::from(ready), published - read, "the bytes ready");
                    let (len, at) = {
                        let len = rng.below(ready + 1);
                        (len, rng.below(ready - len + 1))
                    };
                    assert_eq!(get(&region, rx.span(at, len).unwrap()), expected(read + u64::from(at), len));
                    let n = rng.below(ready + 1);
                    assert_eq!(get(&region, rx.span(0, n).unwrap()), expected(read, n));
                    assert!(rx.advance(n));
                    read += u64::from(n);
                }
                _ => {
                    let _ = rx.release(&page);
                    released = read;
                }
            }
        }
        assert!(read > 4 * u64::from(C), "{read} bytes did not go round the lap twice");
        assert!(region[..BASE].iter().chain(&region[BASE + C as usize..]).all(|&b| b == 0xA5), "a guard was written");
    }

    #[test]
    fn the_ring_agrees_with_a_stream_that_never_wraps() {
        for seed in 1..=16 {
            agrees::<1>(seed);
            agrees::<2>(seed);
            agrees::<5>(seed);
            agrees::<8>(seed);
            agrees::<61>(seed);
        }
    }

    /// A word a peer might write: anything, a position near the lap, or an edge.
    fn hostile<const C: u32>(rng: &mut Rng) -> u32 {
        match rng.below(3) {
            0 => rng.next() as u32,
            1 => rng.below(4 * C + 2),
            _ => [0, C - 1, C, C + 1, 2 * C - 1, 2 * C, 2 * C + 1, u32::MAX, u32::MAX - C][rng.below(9) as usize],
        }
    }

    /// Whatever a producer, or another holder of this end, stores in any of
    /// the four words, the consumer holds at most the ring past what it
    /// released, every span it is handed lies in the ring, a tail it does not
    /// believe is `TailPastDepth` and a head it resumes past the lap is
    /// `OwnCursor`.
    fn hostile_producer<const C: u32>(seed: u64) {
        let page = page();
        let mut rx = Consumer::new(&page, place::<C>());
        let mut rng = Rng(seed);
        let mut held = 0;
        for _ in 0..2000 {
            page[rng.below(4) as usize].store(hostile::<C>(&mut rng), Ordering::Release);
            let seen = match rng.below(5) {
                4 => {
                    match rx.resume(&page) {
                        Ok(()) => held = 0,
                        Err(violation) => assert_eq!(violation, Violation::OwnCursor),
                    }
                    continue;
                }
                0 => rx.ready(&page),
                1 => rx.arm(&page),
                2 => {
                    let n = rng.below(C + 1);
                    match rx.span(0, n) {
                        Some(spans) => {
                            inside::<C>(spans, n);
                            assert!(rx.advance(n));
                            held += n;
                        }
                        None => assert!(!rx.advance(n), "advanced over {n} bytes it was never handed"),
                    }
                    continue;
                }
                _ => {
                    let _ = rx.release(&page);
                    held = 0;
                    continue;
                }
            };
            match seen {
                Ok(ready) => assert!(
                    u64::from(held) + u64::from(ready) <= u64::from(C),
                    "holds {held} and was told of {ready} more in a ring of {C}"
                ),
                Err(violation) => assert_eq!(violation, Violation::TailPastDepth),
            }
        }
    }

    #[test]
    fn a_hostile_producer_hands_the_consumer_at_most_the_ring() {
        for seed in 1..=64 {
            hostile_producer::<1>(seed);
            hostile_producer::<5>(seed);
            hostile_producer::<8>(seed);
        }
    }

    /// Whatever a consumer, or another holder of this end, stores in any of
    /// the four words, the producer writes at most the ring past the head it
    /// was shown, every span it is handed lies in the ring, a head it does not
    /// believe is `HeadPastTail` and a tail it resumes past the lap is
    /// `OwnCursor`.
    fn hostile_consumer<const C: u32>(seed: u64) {
        let page = page();
        let mut tx = Producer::new(&page, place::<C>());
        let mut rng = Rng(seed);
        let mut pending = 0;
        for _ in 0..2000 {
            page[rng.below(4) as usize].store(hostile::<C>(&mut rng), Ordering::Release);
            let seen = match rng.below(5) {
                4 => {
                    match tx.resume(&page) {
                        Ok(()) => pending = 0,
                        Err(violation) => assert_eq!(violation, Violation::OwnCursor),
                    }
                    continue;
                }
                0 => tx.room(&page),
                1 => tx.arm(&page),
                2 => {
                    let n = rng.below(C + 1);
                    match tx.span(0, n) {
                        Some(spans) => {
                            inside::<C>(spans, n);
                            assert!(tx.advance(n));
                            pending += n;
                        }
                        None => assert!(!tx.advance(n), "advanced over {n} bytes it was never handed"),
                    }
                    continue;
                }
                _ => {
                    let _ = tx.publish(&page);
                    pending = 0;
                    continue;
                }
            };
            match seen {
                Ok(room) => assert!(
                    u64::from(pending) + u64::from(room) <= u64::from(C),
                    "{pending} unpublished and {room} more in a ring of {C}"
                ),
                Err(violation) => assert_eq!(violation, Violation::HeadPastTail),
            }
        }
    }

    #[test]
    fn a_hostile_consumer_gives_the_producer_at_most_the_ring() {
        for seed in 1..=64 {
            hostile_consumer::<1>(seed);
            hostile_consumer::<5>(seed);
            hostile_consumer::<8>(seed);
        }
    }

    /// From every position round the lap, twice: a tail the ring ahead is
    /// believed, and one a byte further, a word past the lap or the last word
    /// is not; a head the ring behind is believed, and one a byte past what was
    /// published or a word past the lap is not.
    #[test]
    fn a_cursor_is_believed_up_to_the_ring_and_no_further() {
        const C: u32 = 5;
        const LAP: u32 = 2 * C;
        let page = page();
        let (mut tx, mut rx) = (Producer::new(&page, place::<C>()), Consumer::new(&page, place::<C>()));
        for at in 0..2 * LAP {
            let here = at % LAP;
            let tails = [
                ((here + C) % LAP, Ok(C)),
                ((here + C + 1) % LAP, Err(Violation::TailPastDepth)),
                (LAP, Err(Violation::TailPastDepth)),
                (u32::MAX, Err(Violation::TailPastDepth)),
            ];
            for (tail, believed) in tails {
                page[1].store(tail, Ordering::Release);
                assert_eq!(rx.ready(&page), believed, "tail {tail} with {here} released");
            }
            let heads = [
                ((here + LAP - C) % LAP, Ok(0)),
                ((here + 1) % LAP, Err(Violation::HeadPastTail)),
                (LAP, Err(Violation::HeadPastTail)),
            ];
            for (head, believed) in heads {
                page[0].store(head, Ordering::Release);
                assert_eq!(tx.room(&page), believed, "head {head} with {here} published");
            }
            page[0].store(here, Ordering::Release);
            assert_eq!(tx.room(&page), Ok(C), "an honest head at {here}");
            assert!(tx.advance(1));
            let _ = tx.publish(&page);
            assert_eq!(rx.ready(&page), Ok(1));
            assert!(rx.advance(1));
            let _ = rx.release(&page);
        }
    }

    /// A holder resumes its own cursor from any position below the lap, and
    /// refuses the lap and past it.
    #[test]
    fn a_resumed_cursor_is_believed_below_the_lap_and_no_further() {
        const C: u32 = 5;
        let page = page();
        let (mut tx, mut rx) = (Producer::joining(place::<C>()), Consumer::joining(place::<C>()));
        for at in [0, 1, C, 2 * C - 1] {
            page[0].store(at, Ordering::Release);
            page[1].store(at, Ordering::Release);
            assert_eq!((rx.resume(&page), tx.resume(&page)), (Ok(()), Ok(())), "a cursor at {at}");
            assert_eq!((rx.ready(&page), tx.room(&page)), (Ok(0), Ok(C)), "the ring at {at} is empty");
        }
        for at in [2 * C, 2 * C + 1, u32::MAX] {
            page[0].store(at, Ordering::Release);
            page[1].store(at, Ordering::Release);
            assert_eq!(rx.resume(&page), Err(Violation::OwnCursor), "a head at {at}");
            assert_eq!(tx.resume(&page), Err(Violation::OwnCursor), "a tail at {at}");
        }
    }

    /// A ring back exchanges a want only of a peer whose direction is ready,
    /// so the want of one that is not stays for the move that makes it ready.
    #[test]
    fn a_ring_back_answers_only_a_peer_that_armed_and_is_ready() {
        const C: u32 = 2;
        let page = page();
        let (mut tx, mut rx) = (Producer::new(&page, place::<C>()), Consumer::new(&page, place::<C>()));
        assert_eq!(rx.arm(&page), Ok(0));
        assert_eq!(tx.ring_back(&page, false), Ok(false), "nothing to read: the want stays");
        assert_eq!(tx.ring_back(&page, true), Ok(true), "the ring ended");
        assert_eq!(tx.ring_back(&page, true), Ok(false), "the want was exchanged");
        assert!(tx.advance(2));
        assert!(!tx.publish(&page), "nobody armed");
        assert_eq!(rx.arm(&page), Ok(2));
        assert_eq!(tx.ring_back(&page, false), Ok(true), "bytes past the head");
        assert_eq!(tx.arm(&page), Ok(0), "a full ring");
        assert_eq!(rx.ring_back(&page, false), Ok(false), "no room: the want stays");
        assert_eq!(rx.ring_back(&page, true), Ok(true), "a fatal end");
        assert_eq!(tx.arm(&page), Ok(0));
        assert!(rx.advance(1));
        assert_eq!(rx.ring_back(&page, false), Ok(false), "this end released nothing yet");
        assert!(rx.release(&page), "the release rings it");
        assert_eq!(tx.arm(&page), Ok(1));
        assert_eq!(rx.ring_back(&page, false), Ok(true), "room past what was released");
    }

    /// An end rings exactly when the peer armed since the last ring, and only
    /// when it moved its cursor.
    #[test]
    fn an_end_rings_only_a_peer_that_armed() {
        const C: u32 = 4;
        let page = page();
        let (mut tx, mut rx) = (Producer::new(&page, place::<C>()), Consumer::new(&page, place::<C>()));
        assert!(tx.advance(1));
        assert!(!tx.publish(&page), "nobody armed");
        assert_eq!(rx.arm(&page), Ok(1), "the look after arming finds the byte");
        assert!(!tx.publish(&page), "nothing new to publish");
        assert!(tx.advance(1));
        assert!(tx.publish(&page), "the consumer armed");
        assert!(tx.advance(2));
        assert!(!tx.publish(&page), "the ring cleared the want");
        assert_eq!(tx.arm(&page), Ok(0), "a full ring");
        assert!(rx.ready(&page) == Ok(4) && rx.advance(2));
        assert!(rx.release(&page), "the producer armed");
        assert!(rx.advance(2));
        assert!(!rx.release(&page), "the ring cleared the want");
        assert!(!rx.release(&page), "nothing new to release");
    }
}
