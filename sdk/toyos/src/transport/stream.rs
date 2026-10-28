//! A single-producer ring of at most `C` bytes, its cursors, wants and size
//! among `N` words.
//!
//! **A producer copies bytes in through the [`Span`]s it is handed, then
//! publishes its tail with `Release`; a consumer loads the tail with `Acquire`,
//! then copies the bytes out.** The head goes back the other way. Either end
//! addresses bytes by their offset from its own cursor, so a producer may place
//! a byte anywhere in its room before it advances over it. An end reads the
//! peer's cursor only when it asks for its room or what is ready, and stores
//! its own once a batch.
//!
//! **A cursor is a stream position, counting every byte and wrapping at
//! 2^32**, and the ring's capacity is a power of two, so a byte's offset is
//! its position below the capacity whichever lap the cursor is on. The peer's
//! cursor is believed only as a position at most the ring away from what this
//! end last stored; otherwise [`Violation::TailPastDepth`] or
//! [`Violation::HeadPastTail`] names it. Every span lies in the ring whatever
//! the peer wrote.
//!
//! **The producer alone grows the ring, by doubling, whatever it holds.** The
//! size word holds the capacity and, while a growth is pending, the position
//! it switches at: the old ring past the head the producer last read, past
//! every byte it may have placed. A byte before it keeps its offset below the
//! old capacity and a byte from it on lies below the new one, so no byte
//! moves. Until the consumer's head passes the switch the producer's room is
//! still counted in the old capacity, so a byte from the switch on lands
//! where no unread byte lies: it would have to be a whole old ring past one.
//! The consumer loads the size after the tail, so it never reads a byte by a
//! size older than the byte. A consumer only touches the ring below the
//! capacity it was told, and judges a size before it believes it: a capacity
//! past `C`, or a switch more than the old ring past what it released, is
//! [`Violation::Size`].
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
//! own, and a producer its size, from the words the last holder stored.
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
/// A consumer loads the size after the tail it reads by, so it never maps a
/// byte by an older size than the one it was written by.
const SIZE_AFTER_TAIL: bool = !cfg!(feature = "transport-size-before-tail");

/// `n` as an offset into the region; `Place::new` holds `usize` to every `u32`.
#[allow(clippy::as_conversions)]
const fn bytes(n: u32) -> usize {
    n as usize
}

/// Whether position `at` comes before `switch`: they are within 2^31.
#[allow(clippy::as_conversions, clippy::cast_possible_wrap)]
const fn before(at: u32, switch: u32) -> bool {
    (switch.wrapping_sub(at) as i32) > 0
}

/// A ring's capacity and the growth to it that may be pending.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Size {
    /// Bytes, a power of two.
    cap: u32,
    /// The position the ring doubled to `cap` at, while a byte before it may
    /// be unread.
    switch: Option<u32>,
}

/// The size word: log2 of the capacity in its low five bits, bit 5 for a
/// growth pending, and the switch's low 24 bits from bit 8.
const LOG: u32 = 0x1F;
const PENDING: u32 = 1 << 5;
const SWITCH: u32 = 8;

impl Size {
    /// The capacity the byte at `at` lies under.
    fn cap_at(&self, at: u32) -> u32 {
        match self.switch {
            Some(switch) if before(at, switch) => self.cap / 2,
            _ => self.cap,
        }
    }

    fn word(&self) -> u32 {
        let log = self.cap.trailing_zeros();
        match self.switch {
            Some(switch) => log | PENDING | (switch << SWITCH),
            None => log,
        }
    }

    /// The size `word` says, its switch read as the position within 2^23 of
    /// `near`; `None` for a capacity that is no power of two to `max`, or a
    /// growth to a capacity of one.
    #[allow(clippy::as_conversions, clippy::cast_possible_wrap, clippy::cast_sign_loss)]
    fn of(word: u32, near: u32, max: u32) -> Option<Self> {
        let log = word & LOG;
        let cap = 1u32.checked_shl(log).filter(|&cap| cap <= max)?;
        if word & PENDING == 0 {
            return Some(Self { cap, switch: None });
        }
        if cap < 2 {
            return None;
        }
        let apart = (((word >> SWITCH).wrapping_sub(near) << SWITCH) as i32) >> SWITCH;
        Some(Self { cap, switch: Some(near.wrapping_add(apart as u32)) })
    }
}

/// Where a ring of at most `C` bytes is: its five words among `N` and the
/// region offset of its first byte.
///
/// ```
/// use toyos::transport::stream::Place;
/// const RING: Place<4096, 80> = Place::new::<0, 16, 32, 48, 64, 4096>();
/// ```
///
/// A place with a word at `N` or past it does not compile:
///
/// ```compile_fail,E0080
/// use toyos::transport::stream::Place;
/// const RING: Place<4096, 64> = Place::new::<0, 16, 32, 48, 64, 4096>();
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Place<const C: u32, const N: usize> {
    head: usize,
    tail: usize,
    /// Set by a consumer about to sleep on an empty ring.
    data: usize,
    /// Set by a producer about to sleep on a full ring.
    room: usize,
    /// The producer's [`Size`].
    size: usize,
    base: usize,
}

impl<const C: u32, const N: usize> Place<C, N> {
    pub const fn new<
        const HEAD: usize,
        const TAIL: usize,
        const DATA: usize,
        const ROOM: usize,
        const SIZE: usize,
        const BASE: usize,
    >() -> Self {
        const {
            assert!(C.is_power_of_two() && C <= 1 << 22, "a ring holds a power of two bytes, at most 4 MiB");
            assert!(usize::BITS >= u32::BITS, "a ring offset is a u32");
            assert!(HEAD < N && TAIL < N && DATA < N && ROOM < N && SIZE < N, "a place names a word past its words");
            assert!(
                HEAD != TAIL && HEAD != DATA && HEAD != ROOM && HEAD != SIZE && TAIL != DATA && TAIL != ROOM
                    && TAIL != SIZE && DATA != ROOM && DATA != SIZE && ROOM != SIZE,
                "a ring's five words are five"
            );
            assert!(BASE.checked_add(bytes(C)).is_some(), "a ring ends inside the address space");
        }
        Self { head: HEAD, tail: TAIL, data: DATA, room: ROOM, size: SIZE, base: BASE }
    }

    /// Its five words: head, tail, the two wants and the size.
    pub(super) const fn words(&self) -> [usize; 5] {
        [self.head, self.tail, self.data, self.room, self.size]
    }

    /// The region offset of its first byte.
    pub const fn base(&self) -> usize {
        self.base
    }

    /// How far `to` is past `from`.
    fn distance(from: u32, to: u32) -> u32 {
        to.wrapping_sub(from)
    }

    /// The region's bytes `len` on from position `at` by `size`, in ring
    /// order: at most two before the switch and two after, since what lies
    /// before it is at most the old ring and the rest at most the new.
    /// `None` where they would be more, which no size this end believed makes.
    pub fn spans(&self, size: Size, at: u32, len: u32) -> Option<[Span; 4]> {
        let mut spans = [Span { offset: self.base, len: 0 }; 4];
        let (mut at, mut left) = (at, len);
        for span in &mut spans {
            if left == 0 {
                break;
            }
            let cap = size.cap_at(at);
            let to_switch = match size.switch {
                Some(switch) if before(at, switch) => switch.wrapping_sub(at),
                _ => u32::MAX,
            };
            let slot = at & cap.wrapping_sub(1);
            let run = left.min(to_switch).min(cap.wrapping_sub(slot));
            *span = Span { offset: self.base.wrapping_add(bytes(slot)), len: bytes(run) };
            at = at.wrapping_add(run);
            left = left.wrapping_sub(run);
        }
        (left == 0).then_some(spans)
    }

    /// The size the producer stored, judged by an end at `near`.
    fn size<W: Word>(&self, page: &[W; N], near: u32) -> Result<Size, Violation> {
        Size::of(word(page, self.size).load(Ordering::Acquire), near, C).ok_or(Violation::Size)
    }
}

/// The end of a ring that writes bytes. It holds its cursors and not the
/// region; every call is given the region's words.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Producer<const C: u32, const N: usize> {
    place: Place<C, N>,
    size: Size,
    local: u32,
    published: u32,
    /// Bytes past `local` that may be written, as last seen.
    room: u32,
    /// The head the room was last counted from.
    head: u32,
}

impl<const C: u32, const N: usize> Producer<C, N> {
    /// This end of the ring at `place`, its tail stored 0 and its capacity
    /// `cap`, a power of two to `C`.
    pub fn new<W: Word>(page: &[W; N], place: Place<C, N>, cap: u32) -> Self {
        assert!(cap.is_power_of_two() && cap <= C, "a ring of {cap} bytes, at most {C}");
        let size = Size { cap, switch: None };
        word(page, place.size).store(size.word(), Ordering::Release);
        word(page, place.tail).store(0, Ordering::Release);
        Self { place, size, local: 0, published: 0, room: cap, head: 0 }
    }

    /// The end at `place` as its holders left it, its tail and size resumed
    /// before each use ([`Self::resume`]).
    pub const fn joining(place: Place<C, N>) -> Self {
        Self { place, size: Size { cap: 1, switch: None }, local: 0, published: 0, room: 0, head: 0 }
    }

    /// Takes up the tail and the size the end's last holder stored, nothing
    /// advanced past it. A switch more than the old ring from the tail is no
    /// size a holder stored: `OwnCursor`.
    pub fn resume<W: Word>(&mut self, page: &[W; N]) -> Result<(), Violation> {
        if cfg!(feature = "transport-no-resume") {
            return Ok(());
        }
        let tail = word(page, self.place.tail).load(Ordering::Acquire);
        let size = self.place.size(page, tail).map_err(|_| Violation::OwnCursor)?;
        let far = |switch: u32| Place::<C, N>::distance(switch, tail).min(Place::<C, N>::distance(tail, switch)) > size.cap / 2;
        if size.switch.is_some_and(far) {
            return Err(Violation::OwnCursor);
        }
        *self = Self { size, local: tail, published: tail, room: 0, head: tail, ..*self };
        Ok(())
    }

    /// The capacity it holds now: what a span may reach below the base.
    pub fn capacity(&self) -> u32 {
        self.size.cap
    }

    /// The size its spans are laid out by, for an adapter that copies by
    /// position ([`Place::spans`]).
    pub fn size(&self) -> Size {
        self.size
    }

    /// The most it holds from the head [`Self::room`] last read: the old
    /// capacity while a growth is pending before it.
    pub fn holds(&self) -> u32 {
        self.size.cap_at(self.head)
    }

    /// Whether [`Self::grow`] may double it now: below `C`, and no growth
    /// pending.
    pub fn can_grow(&self) -> bool {
        self.size.cap < C && self.size.switch.is_none()
    }

    /// Double the ring, where [`Self::can_grow`] says so, from the old ring
    /// past the head [`Self::room`] last read: the caller made the region
    /// below twice the capacity memory first. The room grows once the
    /// consumer is past the switch.
    pub fn grow<W: Word>(&mut self, page: &[W; N]) {
        assert!(self.can_grow(), "a ring of {} grown, at most {C} and none pending", self.size.cap);
        let switch = self.head.wrapping_add(self.size.cap);
        self.size = Size { cap: self.size.cap.wrapping_mul(2), switch: Some(switch) };
        word(page, self.place.size).store(self.size.word(), Ordering::Release);
    }

    /// How many bytes past the tail may be written, the consumer's head read
    /// again; a growth the consumer is past is no longer pending.
    pub fn room<W: Word>(&mut self, page: &[W; N]) -> Result<u32, Violation> {
        let head = word(page, self.place.head).load(TAKE_BACK);
        let unreleased = clamp(Place::<C, N>::distance(head, self.published), self.size.cap, Violation::HeadPastTail)?;
        if self.size.switch.is_some_and(|switch| !before(head, switch)) {
            self.size.switch = None;
            word(page, self.place.size).store(self.size.word(), Ordering::Release);
        }
        let cap = if cfg!(feature = "transport-grow-room-now") { self.size.cap } else { self.size.cap_at(head) };
        let pending = Place::<C, N>::distance(self.published, self.local);
        self.room = cap.saturating_sub(unreleased.saturating_add(pending));
        self.head = head;
        Ok(self.room)
    }

    /// Where to copy the `len` bytes `at` past the tail; `None` past the room
    /// last seen.
    pub fn span(&self, at: u32, len: u32) -> Option<[Span; 4]> {
        if at.checked_add(len)? > self.room {
            return None;
        }
        self.place.spans(self.size, self.local.wrapping_add(at), len)
    }

    /// Count `n` more bytes written at the tail; `false`, and nothing counted,
    /// past the room last seen. They are the consumer's once
    /// [`Self::publish`] runs.
    #[must_use]
    pub fn advance(&mut self, n: u32) -> bool {
        let Some(room) = self.room.checked_sub(n) else { return false };
        self.local = self.local.wrapping_add(n);
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

    /// Bytes published that the consumer has not released.
    pub fn held<W: Word>(&self, page: &[W; N]) -> Result<u32, Violation> {
        let head = word(page, self.place.head).load(TAKE_BACK);
        clamp(Place::<C, N>::distance(head, self.published), self.size.cap, Violation::HeadPastTail)
    }

    /// The consumer asked to be rung back: `true`, and its want exchanged for
    /// 0, if it is set and the consumer has bytes past its head or the ring
    /// `ended`. With neither, nothing is exchanged and the want waits for the
    /// next publish.
    pub fn ring_back<W: Word>(&self, page: &[W; N], ended: bool) -> Result<bool, Violation> {
        let held = self.held(page)?;
        Ok((held > 0 || ended) && word(page, self.place.data).swap(0, WAKE) != 0)
    }
}

/// The end of a ring that reads bytes; like [`Producer`], it holds cursors and
/// is given the region's words.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Consumer<const C: u32, const N: usize> {
    place: Place<C, N>,
    /// The size as last judged.
    size: Size,
    local: u32,
    released: u32,
    /// Bytes published past `local`, as last seen.
    ready: u32,
}

impl<const C: u32, const N: usize> Consumer<C, N> {
    /// This end of the ring at `place`, its head stored 0.
    pub fn new<W: Word>(page: &[W; N], place: Place<C, N>) -> Self {
        word(page, place.head).store(0, Ordering::Release);
        Self::joining(place)
    }

    /// The end at `place` as its holders left it, its head resumed before each
    /// use ([`Self::resume`]).
    pub const fn joining(place: Place<C, N>) -> Self {
        Self { place, size: Size { cap: 1, switch: None }, local: 0, released: 0, ready: 0 }
    }

    /// Takes up the head the end's last holder stored, nothing advanced past it.
    pub fn resume<W: Word>(&mut self, page: &[W; N]) -> Result<(), Violation> {
        if cfg!(feature = "transport-no-resume") {
            return Ok(());
        }
        let head = word(page, self.place.head).load(Ordering::Acquire);
        *self = Self { local: head, released: head, ready: 0, ..*self };
        Ok(())
    }

    /// The capacity last judged: what a span may reach below the base, which
    /// the caller makes sure is memory before it copies.
    pub fn capacity(&self) -> u32 {
        self.size.cap
    }

    /// The size last judged, as [`Producer::size`].
    pub fn size(&self) -> Size {
        self.size
    }

    /// The producer's tail and size, judged: the bytes published past what
    /// this end released, and the size they are laid out by.
    fn judged<W: Word>(&self, page: &[W; N]) -> Result<(u32, Size), Violation> {
        let early = if SIZE_AFTER_TAIL { None } else { Some(self.place.size(page, self.released)) };
        let tail = word(page, self.place.tail).load(Ordering::Acquire);
        let size = match early {
            Some(size) => size?,
            None => self.place.size(page, self.released)?,
        };
        if size.switch.is_some_and(|switch| before(self.released, switch) && Place::<C, N>::distance(self.released, switch) > size.cap / 2) {
            return Err(Violation::Size);
        }
        let published = clamp(Place::<C, N>::distance(self.released, tail), size.cap, Violation::TailPastDepth)?;
        Ok((published, size))
    }

    /// How many bytes past the head are published, the producer's tail and
    /// size read again.
    pub fn ready<W: Word>(&mut self, page: &[W; N]) -> Result<u32, Violation> {
        let (published, size) = self.judged(page)?;
        self.size = size;
        self.ready = published.saturating_sub(Place::<C, N>::distance(self.released, self.local));
        Ok(self.ready)
    }

    /// Where to copy the `len` bytes `at` past the head from; `None` past what
    /// was last seen ready.
    pub fn span(&self, at: u32, len: u32) -> Option<[Span; 4]> {
        if at.checked_add(len)? > self.ready {
            return None;
        }
        self.place.spans(self.size, self.local.wrapping_add(at), len)
    }

    /// Count `n` more bytes read at the head; `false`, and nothing counted,
    /// past what was last seen ready. They are the producer's once
    /// [`Self::release`] runs.
    #[must_use]
    pub fn advance(&mut self, n: u32) -> bool {
        let Some(ready) = self.ready.checked_sub(n) else { return false };
        self.local = self.local.wrapping_add(n);
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
        let (unreleased, size) = self.judged(page)?;
        Ok((unreleased < size.cap_at(self.released) || answers) && word(page, self.place.room).swap(0, WAKE) != 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::sync::atomic::AtomicU32;

    const WORDS: usize = 5;
    const SIZE: usize = 4;
    /// Off any alignment, so a span that forgets the base lands outside it.
    const BASE: usize = 3;
    /// Bytes on either side of the ring that no span may reach.
    const GUARD: usize = 3;

    fn page() -> [AtomicU32; WORDS] {
        core::array::from_fn(|_| AtomicU32::new(0))
    }

    fn place<const C: u32>() -> Place<C, WORDS> {
        Place::new::<0, 1, 2, 3, SIZE, BASE>()
    }

    /// A region with the ring, at its most, between two guards.
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

    /// The spans hold `len` bytes and lie below `cap` past the base.
    fn inside(spans: [Span; 4], len: u32, cap: u32) {
        assert_eq!(spans.iter().map(|span| span.len).sum::<usize>(), len as usize, "{spans:?} is not {len} bytes");
        for span in spans.iter().filter(|span| span.len > 0) {
            assert!(span.offset >= BASE && span.offset + span.len <= BASE + cap as usize, "{span:?} leaves a ring of {cap}");
        }
    }

    fn put(region: &mut [u8], spans: [Span; 4], from: u64) {
        let mut at = from;
        for span in spans {
            for b in &mut region[span.offset..span.offset + span.len] {
                *b = byte(at);
                at += 1;
            }
        }
    }

    fn get(region: &[u8], spans: [Span; 4]) -> Vec<u8> {
        spans.iter().flat_map(|span| region[span.offset..span.offset + span.len].iter().copied()).collect()
    }

    fn expected(from: u64, len: u32) -> Vec<u8> {
        (from..from + u64::from(len)).map(byte).collect()
    }

    /// What the producer's room is, by the module's rule: the old capacity
    /// until the head is past a pending switch.
    struct Model {
        cap: u32,
        switch: Option<u64>,
    }

    impl Model {
        fn room(&mut self, written: u64, released: u64) -> u64 {
            match self.switch {
                Some(switch) if released < switch => u64::from(self.cap / 2) - (written - released),
                _ => {
                    self.switch = None;
                    u64::from(self.cap) - (written - released)
                }
            }
        }
    }

    /// The oracle is a stream that never wraps: four counts of bytes since
    /// the start, and the rule the room follows. The producer starts at
    /// `start`, places each batch out of order, and doubles the ring at
    /// random to `C`; the consumer reads at offsets as well as at its head,
    /// and every room, every count ready and every byte read is what the
    /// counts say, round the lap many times. A byte the producer placed and
    /// had not published when it grew is read as it was written.
    fn agrees<const C: u32>(start: u32, seed: u64) {
        let page = page();
        let (mut tx, mut rx) = (Producer::new(&page, place::<C>(), start), Consumer::new(&page, place::<C>()));
        let mut region = region::<C>();
        let (mut written, mut published, mut read, mut released) = (0u64, 0u64, 0u64, 0u64);
        let mut model = Model { cap: start, switch: None };
        let mut rng = Rng(seed);
        for _ in 0..400 * C + 400 {
            match rng.below(5) {
                0 => {
                    let room = tx.room(&page).unwrap();
                    assert_eq!(u64::from(room), model.room(written, released), "the room");
                    let n = rng.below(room + 1);
                    let split = rng.below(n + 1);
                    // The later part is placed first, and a growth may come
                    // between it and the rest.
                    put(&mut region, tx.span(split, n - split).unwrap(), written + u64::from(split));
                    if tx.can_grow() && rng.below(8) == 0 {
                        tx.grow(&page);
                        model = Model { cap: model.cap * 2, switch: Some(released + u64::from(model.cap)) };
                    }
                    put(&mut region, tx.span(0, split).unwrap(), written);
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
                    let spans = rx.span(at, len).unwrap();
                    inside(spans, len, rx.capacity());
                    assert_eq!(get(&region, spans), expected(read + u64::from(at), len));
                    let n = rng.below(ready + 1);
                    assert_eq!(get(&region, rx.span(0, n).unwrap()), expected(read, n));
                    assert!(rx.advance(n));
                    read += u64::from(n);
                }
                3 => {
                    let _ = rx.release(&page);
                    released = read;
                }
                _ => {}
            }
        }
        assert!(read > 4 * u64::from(model.cap), "{read} bytes did not go round the lap twice");
        assert!(region[..BASE].iter().chain(&region[BASE + C as usize..]).all(|&b| b == 0xA5), "a guard was written");
    }

    #[test]
    fn the_ring_agrees_with_a_stream_that_never_wraps() {
        for seed in 1..=16 {
            agrees::<1>(1, seed);
            agrees::<2>(2, seed);
            agrees::<8>(8, seed);
            agrees::<64>(64, seed);
        }
    }

    #[test]
    fn a_ring_that_grows_agrees_with_the_stream_too() {
        for seed in 1..=64 {
            agrees::<64>(1, seed);
            agrees::<256>(4, seed);
        }
    }

    /// The ring never empties: the producer keeps it at least half full and
    /// the consumer reads only what leaves that much behind, so a growth that
    /// waited for an empty ring never comes. It reaches its place, each
    /// doubling within a few old rings of bytes, and every byte is read as
    /// written.
    #[test]
    fn a_ring_kept_full_grows_to_its_place() {
        const C: u32 = 1 << 12;
        let page = page();
        let (mut tx, mut rx) = (Producer::new(&page, place::<C>(), 16), Consumer::new(&page, place::<C>()));
        let mut region = region::<C>();
        let (mut written, mut read) = (0u64, 0u64);
        let mut grew_at = Vec::new();
        while tx.capacity() < C || tx.holds() < C {
            assert!(written < 64 * u64::from(C), "the ring held {} after {written} bytes", tx.holds());
            let room = tx.room(&page).unwrap();
            if tx.can_grow() {
                tx.grow(&page);
                grew_at.push(written);
            }
            put(&mut region, tx.span(0, room).unwrap(), written);
            assert!(tx.advance(room));
            written += u64::from(room);
            let _ = tx.publish(&page);
            let ready = rx.ready(&page).unwrap();
            let n = ready - ready.min(tx.holds() / 2);
            assert_eq!(get(&region, rx.span(0, n).unwrap()), expected(read, n));
            assert!(rx.advance(n));
            read += u64::from(n);
            let _ = rx.release(&page);
            assert!(written > read, "the ring emptied");
        }
        assert_eq!(grew_at.len(), 8, "16 bytes doubled to {C}: {grew_at:?}");
    }

    /// A word a peer might write: anything, a position near the lap, or an edge.
    fn hostile<const C: u32>(rng: &mut Rng) -> u32 {
        match rng.below(3) {
            0 => rng.next() as u32,
            1 => rng.below(4 * C + 2),
            _ => [0, C - 1, C, C + 1, 2 * C - 1, 2 * C, 2 * C + 1, u32::MAX, u32::MAX - C][rng.below(9) as usize],
        }
    }

    /// A size word a peer might write: any capacity log, pending or not, a
    /// switch anywhere.
    fn hostile_size(rng: &mut Rng) -> u32 {
        match rng.below(2) {
            0 => rng.next() as u32,
            _ => rng.below(32) | (rng.below(2) << 5) | ((rng.next() as u32) << 8),
        }
    }

    /// Whatever a producer, or another holder of this end, stores in any of
    /// the five words, the consumer holds at most its capacity past what it
    /// released, which is at most the place's, every span it is handed lies
    /// below it, and what it does not believe is `TailPastDepth` or `Size`.
    fn hostile_producer<const C: u32>(seed: u64) {
        let page = page();
        let mut rx = Consumer::new(&page, place::<C>());
        let mut rng = Rng(seed);
        let mut held = 0;
        for _ in 0..2000 {
            let at = rng.below(5) as usize;
            let value = if at == SIZE { hostile_size(&mut rng) } else { hostile::<C>(&mut rng) };
            page[at].store(value, Ordering::Release);
            let seen = match rng.below(5) {
                4 => {
                    assert_eq!(rx.resume(&page), Ok(()));
                    held = 0;
                    continue;
                }
                0 => rx.ready(&page),
                1 => rx.arm(&page),
                2 => {
                    let n = rng.below(C + 1);
                    match rx.span(0, n) {
                        Some(spans) => {
                            inside(spans, n, rx.capacity());
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
            assert!(rx.capacity() <= C);
            match seen {
                Ok(ready) => assert!(
                    u64::from(held) + u64::from(ready) <= u64::from(rx.capacity()),
                    "holds {held} and was told of {ready} more in a ring of {}",
                    rx.capacity()
                ),
                Err(violation) => assert!(matches!(violation, Violation::TailPastDepth | Violation::Size), "{violation:?}"),
            }
        }
    }

    #[test]
    fn a_hostile_producer_hands_the_consumer_at_most_the_ring() {
        for seed in 1..=64 {
            hostile_producer::<1>(seed);
            hostile_producer::<8>(seed);
            hostile_producer::<64>(seed);
        }
    }

    /// Whatever a consumer, or another holder of this end, stores in any of
    /// the five words, the producer writes at most its capacity past the head
    /// it was shown, every span it is handed lies below it, a head it does not
    /// believe is `HeadPastTail` and a size it resumes that it never stored is
    /// `OwnCursor`.
    fn hostile_consumer<const C: u32>(seed: u64) {
        let page = page();
        let mut tx = Producer::new(&page, place::<C>(), C);
        let mut rng = Rng(seed);
        let mut pending = 0;
        for _ in 0..2000 {
            let at = rng.below(5) as usize;
            let value = if at == SIZE { hostile_size(&mut rng) } else { hostile::<C>(&mut rng) };
            page[at].store(value, Ordering::Release);
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
                            inside(spans, n, tx.capacity());
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
                    u64::from(pending) + u64::from(room) <= u64::from(tx.capacity()),
                    "{pending} unpublished and {room} more in a ring of {}",
                    tx.capacity()
                ),
                Err(violation) => assert_eq!(violation, Violation::HeadPastTail),
            }
        }
    }

    #[test]
    fn a_hostile_consumer_gives_the_producer_at_most_the_ring() {
        for seed in 1..=64 {
            hostile_consumer::<1>(seed);
            hostile_consumer::<8>(seed);
            hostile_consumer::<64>(seed);
        }
    }

    /// From positions on both sides of the u32 wrap: a tail the ring ahead is
    /// believed and one a byte further is not; a head the ring behind is
    /// believed, and one a byte past what was published is not.
    #[test]
    fn a_cursor_is_believed_up_to_the_ring_and_no_further() {
        const C: u32 = 4;
        let page = page();
        let (mut tx, mut rx) = (Producer::new(&page, place::<C>(), C), Consumer::new(&page, place::<C>()));
        let from = u32::MAX - 2 * C;
        page[0].store(from, Ordering::Release);
        page[1].store(from, Ordering::Release);
        assert_eq!((rx.resume(&page), tx.resume(&page)), (Ok(()), Ok(())));
        for _ in 0..4 * C {
            let here = rx.released;
            for (tail, believed) in [(here.wrapping_add(C), Ok(C)), (here.wrapping_add(C + 1), Err(Violation::TailPastDepth))] {
                page[1].store(tail, Ordering::Release);
                assert_eq!(rx.ready(&page), believed, "tail {tail} with {here} released");
            }
            page[1].store(here, Ordering::Release);
            for (head, believed) in [(here.wrapping_sub(C), Ok(0)), (here.wrapping_add(1), Err(Violation::HeadPastTail))] {
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

    /// A producer resumes the size its end's last holder stored, and refuses
    /// one whose switch is further than the old ring from its tail.
    #[test]
    fn a_resumed_size_is_believed_only_near_the_tail() {
        const C: u32 = 16;
        let page = page();
        let mut first = Producer::new(&page, place::<C>(), 4);
        assert_eq!(first.room(&page), Ok(4));
        first.grow(&page);
        let mut next = Producer::joining(place::<C>());
        assert_eq!(next.resume(&page), Ok(()));
        assert_eq!((next.capacity(), next.can_grow()), (8, false), "the growth the first holder made");
        let far = Size { cap: 8, switch: Some(64) }.word();
        page[SIZE].store(far, Ordering::Release);
        assert_eq!(next.resume(&page), Err(Violation::OwnCursor));
    }

    /// A ring back exchanges a want only of a peer whose direction is ready,
    /// so the want of one that is not stays for the move that makes it ready.
    #[test]
    fn a_ring_back_answers_only_a_peer_that_armed_and_is_ready() {
        const C: u32 = 2;
        let page = page();
        let (mut tx, mut rx) = (Producer::new(&page, place::<C>(), C), Consumer::new(&page, place::<C>()));
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
        let (mut tx, mut rx) = (Producer::new(&page, place::<C>(), C), Consumer::new(&page, place::<C>()));
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
