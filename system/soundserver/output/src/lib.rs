//! soundserver's output protocol: an audio driver process, one per function it
//! drives, is a client of soundserver's [`PORT`].
//!
//! **Two connections per driver, both opened on [`PORT`].** On the first, the
//! *control* connection, the driver offers its output ([`Offer`]), and
//! soundserver answers [`MSG_ACCEPTED`] with the ring of periods it lends the
//! driver, [`Offer::periods`] of [`Offer::period_bytes`] laid end to end, or
//! [`MSG_REFUSED`]. The driver points its device at the ring and opens the
//! second, the *stream* connection, whose first frame is [`MSG_STREAM`]: one
//! offer is accepted at a time, so the stream is that offer's. The master's
//! requests and answers stay on the control connection; [`MSG_START`],
//! [`MSG_STOP`] and [`MSG_PLAYED`] are the stream connection's, so
//! soundserver's mix thread reads nothing but periods.
//!
//! **Every frame is decoded here, once**, and one that does not decode is the
//! peer's protocol error: the reader ends the connection by name.

#![no_std]

/// The port a driver connects to; soundserver serves it.
pub const PORT: &str = "sound-output";

/// The rates soundserver mixes at, best first: a driver offers the first its
/// device runs at, and an offer at another is refused.
pub use toyos_mixer::DEVICE_RATES as RATES;

/// The deepest ring an offer may name: the mixer's deepest pipeline.
pub const MAX_PERIODS: usize = toyos_mixer::MAX_PIPELINE;

/// The longest period an offer may name, in bytes: 4096 stereo frames, 93 ms
/// at 44.1 kHz. Policy, not physics: soundserver sizes its mix bus, its
/// scratch and every client's ring from the period, and lends the ring out of
/// its own memory, so a driver's word is bounded here, far past any device's
/// period, before anything is allocated from it.
pub const MAX_PERIOD_BYTES: u32 = 16 * 1024;

// The control connection, driver to soundserver.
pub const MSG_OFFER: u32 = 1;
pub const MSG_STREAM: u32 = 2;
pub const MSG_MASTER: u32 = 3;
pub const MSG_MASTER_UNAVAILABLE: u32 = 4;
// The control connection, soundserver to driver.
pub const MSG_ACCEPTED: u32 = 16;
pub const MSG_REFUSED: u32 = 17;
pub const MSG_MASTER_STEP: u32 = 18;
pub const MSG_MASTER_MUTE: u32 = 19;
// The stream connection.
pub const MSG_START: u32 = 32;
pub const MSG_STOP: u32 = 33;
pub const MSG_PLAYED: u32 = 34;

/// The handles [`MSG_ACCEPTED`] carries: the lent ring.
pub const ACCEPTED_HANDLES: usize = 1;

/// What a driver's device plays, and whether it has a master.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Offer {
    pub rate: u32,
    pub channels: u8,
    pub periods: u8,
    pub period_bytes: u32,
    /// The master's quietest position, where the path has a master.
    pub master_bottom: Option<u8>,
}

impl Offer {
    pub const BYTES: usize = 12;

    pub fn encode(&self) -> [u8; Self::BYTES] {
        let mut out = [0u8; Self::BYTES];
        out[0..4].copy_from_slice(&self.rate.to_le_bytes());
        out[4..8].copy_from_slice(&self.period_bytes.to_le_bytes());
        out[8] = self.channels;
        out[9] = self.periods;
        out[10] = u8::from(self.master_bottom.is_some());
        out[11] = self.master_bottom.unwrap_or(0);
        out
    }

    /// The bytes of a ring of this shape.
    pub fn ring_bytes(&self) -> usize {
        self.periods as usize * self.period_bytes as usize
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let bytes: &[u8; Self::BYTES] = bytes.try_into().ok()?;
        let rate = u32::from_le_bytes(bytes[0..4].try_into().ok()?);
        let period_bytes = u32::from_le_bytes(bytes[4..8].try_into().ok()?);
        let (channels, periods) = (bytes[8], bytes[9]);
        let master_bottom = match (bytes[10], bytes[11]) {
            (0, 0) => None,
            (1, bottom) => Some(bottom),
            _ => return None,
        };
        let frame = 2 * channels as u32;
        let whole = RATES.contains(&rate)
            && matches!(channels, 1 | 2)
            && (2..=MAX_PERIODS).contains(&(periods as usize))
            && periods.is_power_of_two()
            && period_bytes != 0
            && period_bytes <= MAX_PERIOD_BYTES
            && period_bytes.is_multiple_of(frame);
        whole.then_some(Self { rate, channels, periods, period_bytes, master_bottom })
    }
}

/// The master as the codec read it back.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Master {
    /// 0 is the loudest; [`Self::bottom`] the quietest.
    pub position: u8,
    pub bottom: u8,
    pub muted: bool,
}

impl Master {
    pub const BYTES: usize = 3;

    pub fn encode(&self) -> [u8; Self::BYTES] {
        [self.position, self.bottom, u8::from(self.muted)]
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let [position, bottom, muted]: [u8; Self::BYTES] = bytes.try_into().ok()?;
        let muted = match muted {
            0 => false,
            1 => true,
            _ => return None,
        };
        (position <= bottom).then_some(Self { position, bottom, muted })
    }
}

/// Periods the device played, and when its interrupts said so.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Played {
    /// One bit per period, one contiguous run of the ring ([`run`]).
    pub mask: u32,
    /// How many of these periods, in play order, end by [`Self::last_nanos`];
    /// the rest are a later report's to clock.
    pub clocked: u32,
    /// When the first and the newest interrupt this report answers for landed.
    pub first_nanos: u64,
    pub last_nanos: u64,
}

impl Played {
    pub const BYTES: usize = 24;

    pub fn encode(&self) -> [u8; Self::BYTES] {
        let mut out = [0u8; Self::BYTES];
        out[0..4].copy_from_slice(&self.mask.to_le_bytes());
        out[4..8].copy_from_slice(&self.clocked.to_le_bytes());
        out[8..16].copy_from_slice(&self.first_nanos.to_le_bytes());
        out[16..24].copy_from_slice(&self.last_nanos.to_le_bytes());
        out
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let bytes: &[u8; Self::BYTES] = bytes.try_into().ok()?;
        let played = Self {
            mask: u32::from_le_bytes(bytes[0..4].try_into().ok()?),
            clocked: u32::from_le_bytes(bytes[4..8].try_into().ok()?),
            first_nanos: u64::from_le_bytes(bytes[8..16].try_into().ok()?),
            last_nanos: u64::from_le_bytes(bytes[16..24].try_into().ok()?),
        };
        let whole = played.mask != 0
            && played.clocked <= played.mask.count_ones()
            && played.first_nanos <= played.last_nanos;
        whole.then_some(played)
    }
}

/// What a driver says on a connection's first frame, and on its control
/// connection after it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FromDriver {
    Offer(Offer),
    /// The stream connection's first frame.
    Stream,
    Master(Master),
    MasterUnavailable,
}

impl FromDriver {
    /// The frame `msg_type` carrying `payload`, or `None` for one this
    /// protocol does not have.
    pub fn decode(msg_type: u32, payload: &[u8]) -> Option<Self> {
        match msg_type {
            MSG_OFFER => Offer::decode(payload).map(Self::Offer),
            MSG_STREAM if payload.is_empty() => Some(Self::Stream),
            MSG_MASTER => Master::decode(payload).map(Self::Master),
            MSG_MASTER_UNAVAILABLE if payload.is_empty() => Some(Self::MasterUnavailable),
            _ => None,
        }
    }
}

/// What soundserver says on a driver's control connection.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToDriver {
    /// With [`ACCEPTED_HANDLES`] handles.
    Accepted,
    Refused,
    /// Move the master by this many positions; a step unmutes.
    MasterStep(i32),
    /// Mute, or unmute at the level it had.
    MasterMute,
}

impl ToDriver {
    pub fn decode(msg_type: u32, payload: &[u8]) -> Option<Self> {
        match (msg_type, payload.len()) {
            (MSG_ACCEPTED, 0) => Some(Self::Accepted),
            (MSG_REFUSED, 0) => Some(Self::Refused),
            (MSG_MASTER_STEP, 4) => Some(Self::MasterStep(i32::from_le_bytes(payload.try_into().ok()?))),
            (MSG_MASTER_MUTE, 0) => Some(Self::MasterMute),
            _ => None,
        }
    }
}

/// What soundserver says on a stream connection.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Engine {
    Start,
    Stop,
}

impl Engine {
    pub fn decode(msg_type: u32, payload: &[u8]) -> Option<Self> {
        match (msg_type, payload.len()) {
            (MSG_START, 0) => Some(Self::Start),
            (MSG_STOP, 0) => Some(Self::Stop),
            _ => None,
        }
    }
}

/// A [`MSG_PLAYED`] frame, the only one a driver sends on its stream
/// connection.
pub fn played(msg_type: u32, payload: &[u8]) -> Option<Played> {
    (msg_type == MSG_PLAYED).then(|| Played::decode(payload)).flatten()
}

/// What a played mask says about a cyclic engine.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Run {
    /// The engine handed back `count` periods starting at `first`, and is in
    /// `first + count` now.
    Of { first: usize, count: usize },
    /// Every period in the ring. The engine has been round at least once since
    /// the reader last looked, so nothing in the mask says where it is: the
    /// whole pipeline has played out.
    Lapped,
}

/// Read a played mask of a `periods`-deep ring.
///
/// A mask is a set and the engine is a sequence, so the set alone has to say
/// which period the engine returns to *first* — and that is the mask's
/// lowest-numbered bit only while the run does not wrap the ring. `{6, 7, 0,
/// 1}` is played 6, 7, 0, 1, and a reader filling it lowest-index-first writes
/// the later audio into the buffer the engine reaches soonest.
///
/// It is always one contiguous run: a report a reader takes late is the OR of
/// consecutive ones, and those abut. `None` for anything else, which no walk
/// of a ring produces.
pub fn run(mask: u32, periods: usize) -> Option<Run> {
    if !(2..=MAX_PERIODS).contains(&periods) || mask >> periods != 0 {
        return None;
    }
    let count = mask.count_ones() as usize;
    if count == periods {
        return Some(Run::Lapped);
    }
    let first = (0..periods)
        .find(|&i| mask & 1 << i != 0 && mask & 1 << ((i + periods - 1) % periods) == 0)?;
    ((0..count).fold(0u32, |run, i| run | 1 << ((first + i) % periods)) == mask)
        .then_some(Run::Of { first, count })
}

#[cfg(test)]
mod tests {
    use super::*;

    const OFFER: Offer = Offer { rate: 44_100, channels: 2, periods: 8, period_bytes: 512, master_bottom: Some(87) };

    #[test]
    fn every_frame_decodes_back_to_what_was_encoded() {
        assert_eq!(FromDriver::decode(MSG_OFFER, &OFFER.encode()), Some(FromDriver::Offer(OFFER)));
        let bare = Offer { master_bottom: None, ..OFFER };
        assert_eq!(FromDriver::decode(MSG_OFFER, &bare.encode()), Some(FromDriver::Offer(bare)));
        let master = Master { position: 3, bottom: 87, muted: true };
        assert_eq!(FromDriver::decode(MSG_MASTER, &master.encode()), Some(FromDriver::Master(master)));
        assert_eq!(FromDriver::decode(MSG_STREAM, &[]), Some(FromDriver::Stream));
        assert_eq!(FromDriver::decode(MSG_MASTER_UNAVAILABLE, &[]), Some(FromDriver::MasterUnavailable));
        let played = Played { mask: 0b1100_0001, clocked: 1, first_nanos: 5, last_nanos: 9 };
        assert_eq!(super::played(MSG_PLAYED, &played.encode()), Some(played));
        assert_eq!(ToDriver::decode(MSG_MASTER_STEP, &(-3i32).to_le_bytes()), Some(ToDriver::MasterStep(-3)));
        assert_eq!(ToDriver::decode(MSG_ACCEPTED, &[]), Some(ToDriver::Accepted));
        assert_eq!(Engine::decode(MSG_START, &[]), Some(Engine::Start));
        assert_eq!(Engine::decode(MSG_STOP, &[]), Some(Engine::Stop));
    }

    /// An offer soundserver cannot mix into is refused at the frame, field by
    /// field.
    #[test]
    fn an_offer_outside_what_soundserver_mixes_is_refused() {
        let refused = |offer: Offer| assert_eq!(FromDriver::decode(MSG_OFFER, &offer.encode()), None, "{offer:?}");
        refused(Offer { rate: 96_000, ..OFFER });
        refused(Offer { channels: 0, ..OFFER });
        refused(Offer { channels: 3, ..OFFER });
        refused(Offer { periods: 1, ..OFFER });
        refused(Offer { periods: 6, ..OFFER });
        refused(Offer { periods: 32, ..OFFER });
        refused(Offer { period_bytes: MAX_PERIOD_BYTES + 4, ..OFFER });
        refused(Offer { period_bytes: !3, ..OFFER });
        let widest = Offer { periods: MAX_PERIODS as u8, period_bytes: MAX_PERIOD_BYTES, ..OFFER };
        assert_eq!(FromDriver::decode(MSG_OFFER, &widest.encode()), Some(FromDriver::Offer(widest)));
        refused(Offer { period_bytes: 0, ..OFFER });
        refused(Offer { period_bytes: 514, ..OFFER });
        let mut bytes = OFFER.encode();
        bytes[10] = 2;
        assert_eq!(FromDriver::decode(MSG_OFFER, &bytes), None, "a master flag that is no bool");
        let mut bytes = Offer { master_bottom: None, ..OFFER }.encode();
        bytes[11] = 4;
        assert_eq!(FromDriver::decode(MSG_OFFER, &bytes), None, "a bottom with no master");
        assert_eq!(FromDriver::decode(MSG_OFFER, &OFFER.encode()[..11]), None);
    }

    #[test]
    fn a_master_or_a_report_no_device_produces_is_refused() {
        assert_eq!(FromDriver::decode(MSG_MASTER, &[9, 8, 0]), None, "past the bottom");
        assert_eq!(FromDriver::decode(MSG_MASTER, &[0, 8, 2]), None, "muted is no bool");
        let played = Played { mask: 0b11, clocked: 1, first_nanos: 5, last_nanos: 9 };
        let refused = |p: Played| assert_eq!(super::played(MSG_PLAYED, &p.encode()), None, "{p:?}");
        refused(Played { mask: 0, clocked: 0, ..played });
        refused(Played { clocked: 3, ..played });
        refused(Played { first_nanos: 10, ..played });
        assert_eq!(super::played(MSG_START, &played.encode()), None);
    }

    /// A frame of the wrong type or width, in either direction, is no message.
    #[test]
    fn a_frame_this_protocol_does_not_have_is_refused() {
        for msg_type in [0, MSG_ACCEPTED, MSG_START, MSG_PLAYED, u32::MAX] {
            assert_eq!(FromDriver::decode(msg_type, &[]), None);
        }
        assert_eq!(FromDriver::decode(MSG_STREAM, &[0]), None);
        for msg_type in [0, MSG_OFFER, MSG_PLAYED, u32::MAX] {
            assert_eq!(ToDriver::decode(msg_type, &[]), None);
            assert_eq!(Engine::decode(msg_type, &[]), None);
        }
        assert_eq!(ToDriver::decode(MSG_MASTER_STEP, &[1, 2, 3]), None);
        assert_eq!(Engine::decode(MSG_START, &[0]), None);
    }

    #[test]
    fn a_mask_names_the_period_the_engine_returns_to_first() {
        assert_eq!(run(0b0011_1000, 8), Some(Run::Of { first: 3, count: 3 }));
        assert_eq!(run(0b0000_0001, 8), Some(Run::Of { first: 0, count: 1 }));
        // Played 6, 7, 0, 1: the run starts at 6, not at its lowest bit.
        assert_eq!(run(0b1100_0011, 8), Some(Run::Of { first: 6, count: 4 }));
        assert_eq!(run(0xFF, 8), Some(Run::Lapped));
        assert_eq!(run(0xFFFF, 16), Some(Run::Lapped));
    }

    #[test]
    fn a_mask_that_is_not_one_run_is_refused_rather_than_placed() {
        assert_eq!(run(0b0010_0101, 8), None);
        assert_eq!(run(1 << 8, 8), None);
        assert_eq!(run(1, 1), None);
        assert_eq!(run(0, 8), None, "an empty report is no run");
    }
}
