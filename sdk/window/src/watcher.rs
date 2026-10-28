//! The clipboard's watcher, as its one holder reaches it: every copy made in
//! the session, and sets of its own.
//!
//! **It never blocks.** Every send is a `try_send`, and each answer the
//! compositor makes — a copy, a region for a set — arrives on the connection
//! for the holder's own poller ([`Watcher::conn`]) to report.
//!
//! **One notification outstanding.** [`MSG_CLIPBOARD_WATCH`] asks for the
//! next copy, and the watcher asks again only once [`Watcher::poll`] has
//! handed the last one out.
//!
//! **One set in flight**, from [`Watcher::set`] until [`Watched::Answered`]:
//! the holder keeps what it sets next until then, so its sets never fill the
//! compositor's pipe. A set lands only while the watcher's ask is outstanding,
//! which the compositor judges: one made before this end saw a client copy
//! already in its pipe is refused, and the client's copy stays.

use toyos::endow;
use toyos::ipc::{FrameRx, RxStep, TrySendError};
use toyos::shm::{ReadOnly, SharedMemory};
use toyos::Connection;

use crate::{
    copy_fits, ClipboardShmMsg, CreateError, MAX_CLIPBOARD_BYTES, MAX_INLINE_PAYLOAD, MSG_CLIPBOARD_PASTE,
    MSG_CLIPBOARD_PASTE_SHM, MSG_CLIPBOARD_SET, MSG_CLIPBOARD_SET_ANSWERED, MSG_CLIPBOARD_WATCH, MSG_COPY_BEGIN, MSG_COPY_COMMIT,
    MSG_COPY_REGION,
};

/// Why the watch cannot go on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchError {
    /// The exchange with the compositor failed.
    Compositor(CreateError),
    /// The compositor will not take another frame: it is not reading.
    NotReading,
    /// A copy the compositor sent that is not UTF-8.
    NotText,
    /// A copy the compositor sent of more than [`MAX_CLIPBOARD_BYTES`].
    TooLong(usize),
}

impl std::fmt::Display for WatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Compositor(e) => e.fmt(f),
            Self::NotReading => write!(f, "the compositor takes no more frames from the watcher"),
            Self::NotText => write!(f, "the compositor sent a copy that is not UTF-8"),
            Self::TooLong(len) => write!(f, "the compositor sent a copy of {len} bytes, past {MAX_CLIPBOARD_BYTES}"),
        }
    }
}

impl std::error::Error for WatchError {}

impl From<CreateError> for WatchError {
    fn from(e: CreateError) -> Self {
        Self::Compositor(e)
    }
}

impl From<TrySendError> for WatchError {
    fn from(e: TrySendError) -> Self {
        match e {
            TrySendError::Full => Self::NotReading,
            TrySendError::Syscall(e) => Self::Compositor(CreateError::Kernel(e)),
            TrySendError::TooLarge => unreachable!("every frame the watcher sends is within ipc::MAX_FRAME_LEN"),
        }
    }
}

/// What one [`Watcher::poll`] read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Watched {
    /// Nothing whole is there yet.
    Idle,
    /// A client's copy, empty included: the clipboard, and the next copy is
    /// asked for.
    Copy(String),
    /// The region for the set in flight, written and committed.
    Committed,
    /// The set in flight is answered: the next may go.
    Answered,
}

/// The clipboard's watcher.
pub struct Watcher {
    conn: Connection,
    rx: FrameRx<MAX_INLINE_PAYLOAD>,
    /// A set past [`MAX_INLINE_PAYLOAD`] waiting for its region.
    region_owed: Option<String>,
    /// A set is in flight, unanswered.
    in_flight: bool,
}

impl Watcher {
    /// Watch the clipboard through the compositor's port named `port` in this
    /// program's namespace.
    pub fn connect(port: &str) -> Result<Self, WatchError> {
        let conn = endow::service(port).map_err(CreateError::from)?;
        conn.try_signal(MSG_CLIPBOARD_WATCH)?;
        Ok(Self { conn, rx: FrameRx::new(), region_owed: None, in_flight: false })
    }

    /// The connection, for a poller: readable means [`Self::poll`] has a frame
    /// to read.
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Read what the compositor sent, one frame at most.
    pub fn poll(&mut self) -> Result<Watched, WatchError> {
        let (msg_type, len) = match self.rx.pump(&self.conn) {
            RxStep::Idle => return Ok(Watched::Idle),
            RxStep::Eof => return Err(CreateError::CompositorGone.into()),
            RxStep::Malformed => return Err(CreateError::BrokenOff.into()),
            RxStep::Frame { msg_type, payload_len, .. } => (msg_type, payload_len),
        };
        let payload = self.rx.payload(len);
        match msg_type {
            MSG_CLIPBOARD_PASTE => {
                let text = String::from_utf8(payload.to_vec()).map_err(|_| WatchError::NotText)?;
                self.conn.try_signal(MSG_CLIPBOARD_WATCH)?;
                Ok(Watched::Copy(text))
            }
            MSG_CLIPBOARD_PASTE_SHM => {
                let info = toyos::ipc::decode_payload::<ClipboardShmMsg>(payload)
                    .map_err(|_| CreateError::Protocol(msg_type))?;
                let len = info.len as usize;
                if !copy_fits(len) {
                    return Err(WatchError::TooLong(len));
                }
                let [region] = self.rx.take_handles::<1>().ok_or(CreateError::NoHandle)?;
                let region = ReadOnly::lent(region, len).map_err(CreateError::Lent)?;
                let text = String::from_utf8(region.bytes().to_vec()).map_err(|_| WatchError::NotText)?;
                self.conn.try_signal(MSG_CLIPBOARD_WATCH)?;
                Ok(Watched::Copy(text))
            }
            MSG_COPY_REGION if len == 0 => {
                let text = self.region_owed.take().ok_or(CreateError::Protocol(msg_type))?;
                let [region] = self.rx.take_handles::<1>().ok_or(CreateError::NoHandle)?;
                let region = SharedMemory::lent(region, text.len()).map_err(CreateError::Lent)?;
                region.bytes().write(0, text.as_bytes());
                self.conn.try_signal(MSG_COPY_COMMIT)?;
                Ok(Watched::Committed)
            }
            MSG_CLIPBOARD_SET_ANSWERED if len == 0 && self.in_flight && self.region_owed.is_none() => {
                self.in_flight = false;
                Ok(Watched::Answered)
            }
            other => Err(CreateError::Protocol(other).into()),
        }
    }

    /// Set the clipboard to `text`, empty included.
    ///
    /// # Panics
    /// `text` longer than [`MAX_CLIPBOARD_BYTES`], or a set already in flight:
    /// the caller bounds both.
    pub fn set(&mut self, text: String) -> Result<(), WatchError> {
        assert!(text.len() <= MAX_CLIPBOARD_BYTES, "window: a set of {} bytes", text.len());
        assert!(!self.in_flight, "window: a set while one is in flight");
        self.in_flight = true;
        if text.len() <= MAX_INLINE_PAYLOAD {
            return Ok(self.conn.try_send_bytes(MSG_CLIPBOARD_SET, text.as_bytes())?);
        }
        self.conn.try_send(MSG_COPY_BEGIN, &ClipboardShmMsg { len: text.len() as u32 })?;
        self.region_owed = Some(text);
        Ok(())
    }
}
