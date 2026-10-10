//! The sound device's messages (§5.14.6): their codes, and each structure as
//! the little-endian words it is on the wire.
//!
//! **Every structure here is a whole number of `le32` words**, the `u8`
//! fields of one packed into a word in the order the structure lists them,
//! so a message is written into the grant and read out of it as words and no
//! field is ever a byte access of its own.

/// §5.14.2: the queues, by index. The fourth, `rxq`, carries input and is
/// never configured.
pub const CONTROL_QUEUE: u16 = 0;
pub const EVENT_QUEUE: u16 = 1;
pub const TX_QUEUE: u16 = 2;

/// §5.14.6: the request types this driver sends.
pub const R_PCM_INFO: u32 = 0x0100;
pub const R_PCM_SET_PARAMS: u32 = 0x0101;
pub const R_PCM_PREPARE: u32 = 0x0102;
pub const R_PCM_START: u32 = 0x0104;
pub const R_PCM_STOP: u32 = 0x0105;

/// §5.14.6: the event types a device sends.
pub const EVT_JACK_CONNECTED: u32 = 0x1000;
pub const EVT_JACK_DISCONNECTED: u32 = 0x1001;
pub const EVT_PCM_PERIOD_ELAPSED: u32 = 0x1100;
pub const EVT_PCM_XRUN: u32 = 0x1101;

/// §5.14.6: the status codes, and the only four a device answers.
pub const S_OK: u32 = 0x8000;
pub const S_BAD_MSG: u32 = 0x8001;
pub const S_NOT_SUPP: u32 = 0x8002;
pub const S_IO_ERR: u32 = 0x8003;

/// §5.14.6: `VIRTIO_SND_D_OUTPUT`.
pub const D_OUTPUT: u8 = 0;

/// §5.14.6.6.2: the one sample format this driver writes, and the two rates
/// it can encode, as bit numbers of `formats` and `rates`.
pub const FMT_S16: u8 = 5;
pub const RATE_44100: u8 = 6;
pub const RATE_48000: u8 = 7;

/// Bytes of each structure.
pub const HDR_BYTES: u32 = 4;
pub const QUERY_INFO_BYTES: u32 = 16;
pub const PCM_HDR_BYTES: u32 = 8;
pub const PCM_INFO_BYTES: u32 = 32;
pub const SET_PARAMS_BYTES: u32 = 24;
pub const XFER_BYTES: u32 = 4;
pub const STATUS_BYTES: u32 = 8;
pub const EVENT_BYTES: u32 = 8;

/// A control request this driver sends, by what it is called.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    PcmInfo,
    SetParams,
    Prepare,
    Start,
    Stop,
}

impl Request {
    pub fn name(self) -> &'static str {
        match self {
            Self::PcmInfo => "PCM_INFO",
            Self::SetParams => "PCM_SET_PARAMS",
            Self::Prepare => "PCM_PREPARE",
            Self::Start => "PCM_START",
            Self::Stop => "PCM_STOP",
        }
    }
}

/// A status a device answered that was not `S_OK`, by its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failed {
    BadMsg,
    NotSupp,
    IoErr,
}

impl Failed {
    pub fn name(self) -> &'static str {
        match self {
            Self::BadMsg => "VIRTIO_SND_S_BAD_MSG",
            Self::NotSupp => "VIRTIO_SND_S_NOT_SUPP",
            Self::IoErr => "VIRTIO_SND_S_IO_ERR",
        }
    }
}

/// A status word: `Ok(Ok(()))` for `S_OK`, `Ok(Err(_))` for one of the three
/// failures, and `Err(word)` for anything §5.14.6 does not define.
pub fn status(word: u32) -> Result<Result<(), Failed>, u32> {
    match word {
        S_OK => Ok(Ok(())),
        S_BAD_MSG => Ok(Err(Failed::BadMsg)),
        S_NOT_SUPP => Ok(Err(Failed::NotSupp)),
        S_IO_ERR => Ok(Err(Failed::IoErr)),
        other => Err(other),
    }
}

/// §5.14.6.1: `virtio_snd_query_info` asking for `count` PCM streams from
/// `start_id`, each answered as one [`PCM_INFO_BYTES`] structure.
pub fn pcm_info_query(start_id: u32, count: u32) -> [u32; 4] {
    [R_PCM_INFO, start_id, count, PCM_INFO_BYTES]
}

/// §5.14.6.6: `virtio_snd_pcm_hdr`, which is the whole of a PREPARE, START or
/// STOP.
pub fn pcm_hdr(code: u32, stream_id: u32) -> [u32; 2] {
    [code, stream_id]
}

/// §5.14.6.6.3: `virtio_snd_pcm_set_params`, no feature selected and the
/// padding byte 0 (§5.14.6.6.3.2).
pub fn set_params(stream_id: u32, params: Params) -> [u32; 6] {
    let Params { buffer_bytes, period_bytes, channels, format, rate } = params;
    [
        R_PCM_SET_PARAMS,
        stream_id,
        buffer_bytes,
        period_bytes,
        0,
        u32::from_le_bytes([channels, format, rate, 0]),
    ]
}

/// What a SET_PARAMS selects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Params {
    pub buffer_bytes: u32,
    pub period_bytes: u32,
    pub channels: u8,
    pub format: u8,
    pub rate: u8,
}

/// §5.14.6.6.2: one stream's `virtio_snd_pcm_info`, every field the device's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PcmInfo {
    pub hda_fn_nid: u32,
    pub features: u32,
    pub formats: u64,
    pub rates: u64,
    pub direction: u8,
    pub channels_min: u8,
    pub channels_max: u8,
}

impl PcmInfo {
    /// The structure's eight words, as the device wrote them.
    pub fn decode(words: [u32; 8]) -> Self {
        let [direction, channels_min, channels_max, _] = words[6].to_le_bytes();
        Self {
            hda_fn_nid: words[0],
            features: words[1],
            formats: words[2] as u64 | (words[3] as u64) << 32,
            rates: words[4] as u64 | (words[5] as u64) << 32,
            direction,
            channels_min,
            channels_max,
        }
    }
}

/// §5.14.6: `virtio_snd_event`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    pub code: u32,
    pub data: u32,
}

impl Event {
    /// The event's name, or `None` for a type §5.14.6 does not define.
    pub fn name(self) -> Option<&'static str> {
        match self.code {
            EVT_JACK_CONNECTED => Some("jack connected"),
            EVT_JACK_DISCONNECTED => Some("jack disconnected"),
            EVT_PCM_PERIOD_ELAPSED => Some("period elapsed"),
            EVT_PCM_XRUN => Some("PCM XRUN"),
            _ => None,
        }
    }
}
