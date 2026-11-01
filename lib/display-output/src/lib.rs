//! The compositor's display-output protocol: a display driver process, one
//! per GPU function, is a client of the compositor's [`PORT`].
//!
//! **One connection per output.** The driver's first frame is [`Hello`], the
//! output it scans out. From then on the compositor sends [`Present`] when
//! the frame it composed into the output changed, naming where, and the
//! driver answers [`Presented`] once that frame reached the output's next
//! vertical blank. **One present is outstanding at a time**: damage the
//! compositor composes meanwhile is merged ([`Presenting`]) and sent when the
//! answer arrives, so the compositor never waits on a driver and a driver
//! never queues frames.
//!
//! **Every frame is decoded here, once**, and one that does not decode is the
//! peer's protocol error: the reader ends the connection by name.

#![no_std]

/// The port a driver connects to; the compositor serves it.
pub const PORT: &str = "display-output";

/// The most rectangles one present names. Damage past it is merged into its
/// bounding box, which is what selective update takes anyway (one region per
/// frame, Vol 12 "Selective Fetch").
pub const MAX_RECTS: usize = 8;

// Driver to compositor.
pub const MSG_HELLO: u32 = 1;
pub const MSG_PRESENTED: u32 = 2;
// Compositor to driver.
pub const MSG_PRESENT: u32 = 16;

/// The widest frame either side sends: a present of [`MAX_RECTS`].
pub const MAX_FRAME: usize = Present::HEAD + MAX_RECTS * Rect::BYTES;

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("four bytes"))
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("eight bytes"))
}

/// An output's size in pixels, both nonzero.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Size {
    pub width: u32,
    pub height: u32,
}

/// A rectangle of an output's pixels, nonempty: columns `x..x + width`, rows
/// `y..y + height`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub const BYTES: usize = 16;

    fn right(&self) -> u32 {
        self.x + self.width
    }

    fn bottom(&self) -> u32 {
        self.y + self.height
    }

    /// The smallest rectangle covering both.
    pub fn union(&self, other: &Self) -> Self {
        let (x, y) = (self.x.min(other.x), self.y.min(other.y));
        Self { x, y, width: self.right().max(other.right()) - x, height: self.bottom().max(other.bottom()) - y }
    }

    fn encode(&self, out: &mut [u8]) {
        for (n, v) in [self.x, self.y, self.width, self.height].into_iter().enumerate() {
            out[4 * n..4 * n + 4].copy_from_slice(&v.to_le_bytes());
        }
    }

    /// One within `size`, or `None`.
    fn decode(bytes: &[u8], size: Size) -> Option<Self> {
        let r = Self { x: u32_at(bytes, 0), y: u32_at(bytes, 4), width: u32_at(bytes, 8), height: u32_at(bytes, 12) };
        let inside = r.width != 0
            && r.height != 0
            && r.x.checked_add(r.width).is_some_and(|right| right <= size.width)
            && r.y.checked_add(r.height).is_some_and(|bottom| bottom <= size.height);
        inside.then_some(r)
    }
}

/// The parts of an output a present changed: one to [`MAX_RECTS`]
/// rectangles.
#[derive(Clone, Copy, Debug)]
pub struct Damage {
    rects: [Rect; MAX_RECTS],
    len: usize,
}

impl Damage {
    pub fn of(rect: Rect) -> Self {
        Self { rects: [rect; MAX_RECTS], len: 1 }
    }

    pub fn rects(&self) -> &[Rect] {
        &self.rects[..self.len]
    }

    /// The smallest rectangle covering every one.
    pub fn bounds(&self) -> Rect {
        self.rects().iter().fold(self.rects[0], |all, r| all.union(r))
    }

    /// `rect` added: past [`MAX_RECTS`], every rectangle is merged into their
    /// bounding box.
    pub fn add(&mut self, rect: Rect) {
        if self.len == MAX_RECTS {
            *self = Self::of(self.bounds().union(&rect));
        } else {
            self.rects[self.len] = rect;
            self.len += 1;
        }
    }
}

impl PartialEq for Damage {
    fn eq(&self, other: &Self) -> bool {
        self.rects() == other.rects()
    }
}

impl Eq for Damage {}

/// A driver's first frame: the output it scans out.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Hello {
    pub size: Size,
}

impl Hello {
    pub const BYTES: usize = 8;

    pub fn encode(&self) -> [u8; Self::BYTES] {
        let mut out = [0u8; Self::BYTES];
        out[0..4].copy_from_slice(&self.size.width.to_le_bytes());
        out[4..8].copy_from_slice(&self.size.height.to_le_bytes());
        out
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let bytes: &[u8; Self::BYTES] = bytes.try_into().ok()?;
        let size = Size { width: u32_at(bytes, 0), height: u32_at(bytes, 4) };
        (size.width != 0 && size.height != 0).then_some(Self { size })
    }
}

/// The frame numbered `sequence` is composed into the output; `damage` is
/// what changed since the one before.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Present {
    pub sequence: u64,
    pub damage: Damage,
}

impl Present {
    const HEAD: usize = 8;

    /// The frame's bytes in `out`, and how many there are.
    pub fn encode(&self, out: &mut [u8; MAX_FRAME]) -> usize {
        out[0..8].copy_from_slice(&self.sequence.to_le_bytes());
        for (n, r) in self.damage.rects().iter().enumerate() {
            r.encode(&mut out[Self::HEAD + n * Rect::BYTES..]);
        }
        Self::HEAD + self.damage.len * Rect::BYTES
    }

    /// A present to an output of `size`: every rectangle inside it.
    pub fn decode(msg_type: u32, payload: &[u8], size: Size) -> Option<Self> {
        let rects = payload.get(Self::HEAD..).filter(|_| msg_type == MSG_PRESENT)?;
        let (rects, rest) = rects.as_chunks::<{ Rect::BYTES }>();
        if !rest.is_empty() || !(1..=MAX_RECTS).contains(&rects.len()) {
            return None;
        }
        let mut damage = Damage::of(Rect::decode(&rects[0], size)?);
        for r in &rects[1..] {
            damage.add(Rect::decode(r, size)?);
        }
        Some(Self { sequence: u64_at(payload, 0), damage })
    }
}

/// The present numbered `sequence` reached the output in the vertical blank
/// at `vblank_nanos` (since boot); the output refreshes every
/// `refresh_interval_nanos`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Presented {
    pub sequence: u64,
    pub vblank_nanos: u64,
    pub refresh_interval_nanos: u64,
}

impl Presented {
    pub const BYTES: usize = 24;

    pub fn encode(&self) -> [u8; Self::BYTES] {
        let mut out = [0u8; Self::BYTES];
        out[0..8].copy_from_slice(&self.sequence.to_le_bytes());
        out[8..16].copy_from_slice(&self.vblank_nanos.to_le_bytes());
        out[16..24].copy_from_slice(&self.refresh_interval_nanos.to_le_bytes());
        out
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let bytes: &[u8; Self::BYTES] = bytes.try_into().ok()?;
        let p = Self { sequence: u64_at(bytes, 0), vblank_nanos: u64_at(bytes, 8), refresh_interval_nanos: u64_at(bytes, 16) };
        (p.refresh_interval_nanos != 0).then_some(p)
    }
}

/// What a driver sends.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FromDriver {
    /// The connection's first frame.
    Hello(Hello),
    Presented(Presented),
}

impl FromDriver {
    /// The frame `msg_type` carrying `payload`, or `None` for one this
    /// protocol does not have.
    pub fn decode(msg_type: u32, payload: &[u8]) -> Option<Self> {
        match msg_type {
            MSG_HELLO => Hello::decode(payload).map(Self::Hello),
            MSG_PRESENTED => Presented::decode(payload).map(Self::Presented),
            _ => None,
        }
    }
}

/// A [`Presented`] that answers no outstanding present: the driver's protocol
/// error.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Unasked {
    pub sequence: u64,
    pub outstanding: Option<u64>,
}

/// The compositor's side of one output: the one present outstanding, and the
/// damage owed the next.
#[derive(Debug)]
pub struct Presenting {
    next: u64,
    outstanding: Option<u64>,
    owed: Option<Damage>,
}

impl Default for Presenting {
    fn default() -> Self {
        Self::new()
    }
}

impl Presenting {
    pub fn new() -> Self {
        Self { next: 1, outstanding: None, owed: None }
    }

    /// `rects` of the output changed: the present to send now, or `None`
    /// while one is outstanding, the damage merged into the next.
    pub fn damaged(&mut self, rects: impl IntoIterator<Item = Rect>) -> Option<Present> {
        for rect in rects {
            match &mut self.owed {
                Some(damage) => damage.add(rect),
                None => self.owed = Some(Damage::of(rect)),
            }
        }
        self.send()
    }

    /// The driver answered `presented`: the present owed since, if any.
    pub fn presented(&mut self, presented: &Presented) -> Result<Option<Present>, Unasked> {
        if self.outstanding != Some(presented.sequence) {
            return Err(Unasked { sequence: presented.sequence, outstanding: self.outstanding });
        }
        self.outstanding = None;
        Ok(self.send())
    }

    fn send(&mut self) -> Option<Present> {
        if self.outstanding.is_some() {
            return None;
        }
        let damage = self.owed.take()?;
        let sequence = self.next;
        self.next += 1;
        self.outstanding = Some(sequence);
        Some(Present { sequence, damage })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HD: Size = Size { width: 1920, height: 1080 };

    fn rect(x: u32, y: u32, width: u32, height: u32) -> Rect {
        Rect { x, y, width, height }
    }

    fn encoded(present: &Present) -> ([u8; MAX_FRAME], usize) {
        let mut out = [0u8; MAX_FRAME];
        let n = present.encode(&mut out);
        (out, n)
    }

    #[test]
    fn every_frame_decodes_back_to_what_was_encoded() {
        let hello = Hello { size: HD };
        assert_eq!(FromDriver::decode(MSG_HELLO, &hello.encode()), Some(FromDriver::Hello(hello)));
        let presented = Presented { sequence: 7, vblank_nanos: 1_000, refresh_interval_nanos: 16_666_667 };
        assert_eq!(FromDriver::decode(MSG_PRESENTED, &presented.encode()), Some(FromDriver::Presented(presented)));
        let mut damage = Damage::of(rect(0, 0, 1920, 1080));
        for n in 1..MAX_RECTS as u32 {
            damage.add(rect(n, n, 1, 1));
        }
        for present in [Present { sequence: 1, damage: Damage::of(rect(10, 20, 30, 40)) }, Present { sequence: u64::MAX, damage }] {
            let (bytes, n) = encoded(&present);
            assert_eq!(Present::decode(MSG_PRESENT, &bytes[..n], HD), Some(present));
        }
    }

    /// A rectangle a present names is inside the output the driver said, and
    /// not empty; a present names one to `MAX_RECTS` of them, whole.
    #[test]
    fn a_present_outside_its_output_or_of_no_whole_rectangles_is_refused() {
        let refused = |r: Rect| {
            let (bytes, n) = encoded(&Present { sequence: 1, damage: Damage::of(r) });
            assert_eq!(Present::decode(MSG_PRESENT, &bytes[..n], HD), None, "{r:?}");
        };
        refused(rect(0, 0, 0, 1));
        refused(rect(0, 0, 1, 0));
        refused(rect(1, 0, 1920, 1));
        refused(rect(0, 1080, 1, 1));
        refused(rect(u32::MAX, 0, 2, 1));
        refused(rect(0, u32::MAX, 1, 2));
        let edge = rect(1919, 1079, 1, 1);
        let (bytes, n) = encoded(&Present { sequence: 1, damage: Damage::of(edge) });
        assert!(Present::decode(MSG_PRESENT, &bytes[..n], HD).is_some());
        assert_eq!(Present::decode(MSG_PRESENT, &bytes[..n - 1], HD), None, "a partial rectangle");
        assert_eq!(Present::decode(MSG_PRESENT, &bytes[..8], HD), None, "no rectangle");
        assert_eq!(Present::decode(MSG_PRESENT, &bytes[..4], HD), None, "no sequence");
        assert_eq!(Present::decode(MSG_HELLO, &bytes[..n], HD), None, "another frame's type");
        let mut wide = [0u8; MAX_FRAME + Rect::BYTES];
        for n in 0..=MAX_RECTS {
            edge.encode(&mut wide[Present::HEAD + n * Rect::BYTES..]);
        }
        assert_eq!(Present::decode(MSG_PRESENT, &wide, HD), None, "more than MAX_RECTS");
    }

    #[test]
    fn a_frame_no_driver_sends_is_refused() {
        assert_eq!(FromDriver::decode(MSG_HELLO, &Hello { size: Size { width: 0, height: 1080 } }.encode()), None);
        assert_eq!(FromDriver::decode(MSG_HELLO, &Hello { size: Size { width: 1920, height: 0 } }.encode()), None);
        assert_eq!(FromDriver::decode(MSG_HELLO, &Hello { size: HD }.encode()[..7]), None);
        let presented = Presented { sequence: 1, vblank_nanos: 5, refresh_interval_nanos: 0 };
        assert_eq!(FromDriver::decode(MSG_PRESENTED, &presented.encode()), None, "no refresh interval");
        for msg_type in [0, MSG_PRESENT, u32::MAX] {
            assert_eq!(FromDriver::decode(msg_type, &Hello { size: HD }.encode()), None);
        }
    }

    #[test]
    fn damage_past_the_most_rectangles_merges_into_their_bounds() {
        let mut damage = Damage::of(rect(0, 0, 1, 1));
        for n in 1..MAX_RECTS as u32 {
            damage.add(rect(10 * n, 0, 1, 1));
        }
        assert_eq!(damage.rects().len(), MAX_RECTS);
        damage.add(rect(5, 500, 2, 2));
        assert_eq!(damage.rects(), &[rect(0, 0, 71, 502)]);
        assert_eq!(damage.bounds(), rect(0, 0, 71, 502));
    }

    #[test]
    fn one_present_is_outstanding_and_the_damage_meanwhile_is_the_next() {
        let mut presenting = Presenting::new();
        let first = presenting.damaged([rect(0, 0, 10, 10)]).expect("nothing is outstanding");
        assert_eq!((first.sequence, first.damage.rects()), (1, &[rect(0, 0, 10, 10)][..]));
        assert_eq!(presenting.damaged([rect(100, 0, 5, 5)]), None);
        assert_eq!(presenting.damaged([rect(0, 100, 5, 5)]), None);
        let answer = |sequence| Presented { sequence, vblank_nanos: 1, refresh_interval_nanos: 16_666_667 };
        let second = presenting.presented(&answer(1)).expect("1 is outstanding").expect("damage is owed");
        assert_eq!((second.sequence, second.damage.rects()), (2, &[rect(100, 0, 5, 5), rect(0, 100, 5, 5)][..]));
        assert_eq!(presenting.presented(&answer(2)), Ok(None), "nothing is owed");
        assert_eq!(presenting.damaged([]), None, "no damage is no present");
        assert_eq!(presenting.damaged([rect(1, 1, 1, 1)]).map(|p| p.sequence), Some(3));
    }

    #[test]
    fn an_answer_to_no_outstanding_present_is_the_drivers_error() {
        let mut presenting = Presenting::new();
        let answer = |sequence| Presented { sequence, vblank_nanos: 1, refresh_interval_nanos: 16_666_667 };
        assert_eq!(presenting.presented(&answer(1)), Err(Unasked { sequence: 1, outstanding: None }));
        presenting.damaged([rect(0, 0, 1, 1)]);
        assert_eq!(presenting.presented(&answer(2)), Err(Unasked { sequence: 2, outstanding: Some(1) }));
        assert_eq!(presenting.presented(&answer(1)), Ok(None));
        assert_eq!(presenting.presented(&answer(1)), Err(Unasked { sequence: 1, outstanding: None }), "answered twice");
    }
}
