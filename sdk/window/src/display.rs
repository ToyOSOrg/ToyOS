//! The compositor's `display` port: the mode and the scale, read and set.
//!
//! **One request per connection**, answered once with [`MSG_STATE`] or
//! [`MSG_REFUSED`] and closed. A request the protocol cannot carry — a
//! payload of another length, a scale outside [`Scale`]'s range — is not
//! answered: the compositor drops the connection by name.

use toyos::endow::{self, EndowError};
use toyos::ipc::{self, FrameRx, IpcError};
use toyos::Connection;
use toyos_abi::syscall::SyscallError;

use crate::Scale;

/// Holder → compositor, bare: what is shown.
pub const MSG_GET: u32 = 1;
/// Holder → compositor, a [`ModeRequest`]: take this mode.
pub const MSG_SET_MODE: u32 = 2;
/// Holder → compositor, a [`ScaleRequest`]: draw at this scale.
pub const MSG_SET_SCALE: u32 = 3;

/// Compositor → holder, a [`State`]: what is shown now.
pub const MSG_STATE: u32 = 1;
/// Compositor → holder, a [`Refusal`]: what was asked is not done, and
/// what is shown did not change.
pub const MSG_REFUSED: u32 = 2;

/// [`ScaleRequest::scale`] asking for the scale the panel's size gives, or
/// the image's own where the panel gives none.
pub const SCALE_AUTO: u32 = 0;

toyos::ipc_payload! {
    pub struct ModeRequest {
        pub width: u32,
        pub height: u32,
    }

    /// A [`Scale::wire`], or [`SCALE_AUTO`].
    pub struct ScaleRequest {
        pub scale: u32,
    }

    /// The mode in pixels, the scale as [`Scale::wire`], and the scale kept
    /// as the person's own choice as the same, or [`SCALE_AUTO`].
    pub struct State {
        pub width: u32,
        pub height: u32,
        pub scale: u32,
        pub kept: u32,
    }

    pub struct Refusal {
        pub reason: u32,
    }
}

/// [`Refusal`]'s reasons.
pub const REFUSED_NOT_A_STEP: u32 = 1;
pub const REFUSED_BELOW_LEAST: u32 = 2;
pub const REFUSED_NO_MODES: u32 = 3;
pub const REFUSED_OUT_OF_BOUNDS: u32 = 4;
pub const REFUSED_DECLINED: u32 = 5;

/// What a scale is set to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Setting {
    /// The panel's, or the image's where the panel gives none.
    Auto,
    /// This one, kept across boots.
    Kept(Scale),
}

impl Setting {
    pub fn wire(self) -> u32 {
        match self {
            Self::Auto => SCALE_AUTO,
            Self::Kept(scale) => scale.wire(),
        }
    }

    /// A setting off the wire; `None` for a scale the protocol does not carry.
    pub fn from_wire(n: u32) -> Option<Self> {
        match n {
            SCALE_AUTO => Some(Self::Auto),
            n => Scale::from_wire(n).map(Self::Kept),
        }
    }
}

/// What the compositor shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shown {
    pub width: u32,
    pub height: u32,
    pub scale: Scale,
    pub setting: Setting,
}

impl Shown {
    pub fn wire(self) -> State {
        State { width: self.width, height: self.height, scale: self.scale.wire(), kept: self.setting.wire() }
    }

    fn from_wire(state: State) -> Option<Self> {
        Some(Self {
            width: state.width,
            height: state.height,
            scale: Scale::from_wire(state.scale)?,
            setting: Setting::from_wire(state.kept)?,
        })
    }
}

/// Why the compositor did not do what was asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The scale is not a quarter step from 1 to 4.
    NotAStep,
    /// At that scale the mode would hold less than the least logical screen.
    BelowLeast,
    /// This display's mode is the firmware's, and none other is set.
    NoModes,
    /// The mode is outside the bounds every mode is held to.
    OutOfBounds,
    /// The display declined the mode.
    Declined,
    /// A reason this build does not know.
    Other(u32),
}

impl Refused {
    pub fn wire(self) -> u32 {
        match self {
            Self::NotAStep => REFUSED_NOT_A_STEP,
            Self::BelowLeast => REFUSED_BELOW_LEAST,
            Self::NoModes => REFUSED_NO_MODES,
            Self::OutOfBounds => REFUSED_OUT_OF_BOUNDS,
            Self::Declined => REFUSED_DECLINED,
            Self::Other(reason) => reason,
        }
    }

    fn from_wire(reason: u32) -> Self {
        match reason {
            REFUSED_NOT_A_STEP => Self::NotAStep,
            REFUSED_BELOW_LEAST => Self::BelowLeast,
            REFUSED_NO_MODES => Self::NoModes,
            REFUSED_OUT_OF_BOUNDS => Self::OutOfBounds,
            REFUSED_DECLINED => Self::Declined,
            other => Self::Other(other),
        }
    }
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAStep => write!(f, "a scale is a quarter step from 1 to 4"),
            Self::BelowLeast => write!(f, "the screen would hold less than an 80x24 terminal"),
            Self::NoModes => write!(f, "this display shows the firmware's mode and sets no other"),
            Self::OutOfBounds => write!(f, "the mode is outside the bounds every mode is held to"),
            Self::Declined => write!(f, "the display declined the mode"),
            Self::Other(reason) => write!(f, "refused for a reason this build does not know ({reason})"),
        }
    }
}

/// Why an exchange on the port ended without an answer to act on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// This program was given no such port.
    NotEndowed,
    /// The compositor exited.
    Gone,
    /// The kernel refused a call of the exchange.
    Kernel(SyscallError),
    /// The compositor closed the connection, or sent what this end cannot
    /// read.
    BrokenOff,
    /// The compositor answered with a message this exchange does not allow.
    Protocol(u32),
    Refused(Refused),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotEndowed => write!(f, "this program was given no display port"),
            Self::Gone => write!(f, "the compositor is gone"),
            Self::Kernel(e) => write!(f, "the kernel refused the exchange ({e:?})"),
            Self::BrokenOff => write!(f, "the compositor broke off the exchange"),
            Self::Protocol(msg_type) => write!(f, "the compositor answered with message {msg_type}"),
            Self::Refused(why) => why.fmt(f),
        }
    }
}

impl std::error::Error for Error {}

impl From<EndowError> for Error {
    fn from(e: EndowError) -> Self {
        match e {
            EndowError::NotEndowed => Self::NotEndowed,
            EndowError::ServerGone => Self::Gone,
            EndowError::Refused(e) => Self::Kernel(e),
        }
    }
}

impl From<IpcError> for Error {
    fn from(e: IpcError) -> Self {
        match e {
            IpcError::Disconnected | IpcError::Malformed => Self::BrokenOff,
            IpcError::Syscall(e) => Self::Kernel(e),
            IpcError::TooLarge => unreachable!("every request here is within ipc::MAX_FRAME_LEN"),
        }
    }
}

/// What the display on `port` shows.
pub fn get(port: &str) -> Result<Shown, Error> {
    ask(port, |conn| conn.signal(MSG_GET))
}

/// Ask the display on `port` for `width` by `height` pixels.
pub fn set_mode(port: &str, width: u32, height: u32) -> Result<Shown, Error> {
    ask(port, |conn| conn.send(MSG_SET_MODE, &ModeRequest { width, height }))
}

/// Ask the display on `port` to draw at `setting`.
pub fn set_scale(port: &str, setting: Setting) -> Result<Shown, Error> {
    ask(port, |conn| conn.send(MSG_SET_SCALE, &ScaleRequest { scale: setting.wire() }))
}

fn ask(port: &str, request: impl FnOnce(&Connection) -> Result<(), IpcError>) -> Result<Shown, Error> {
    let conn = endow::service(port)?;
    request(&conn)?;
    let mut rx = FrameRx::<{ core::mem::size_of::<State>() }>::new();
    let frame = rx.recv(&conn)?;
    let payload = rx.payload(frame.payload_len);
    match frame.msg_type {
        MSG_STATE => {
            let state: State = ipc::decode_payload(payload)?;
            Shown::from_wire(state).ok_or(Error::Protocol(MSG_STATE))
        }
        MSG_REFUSED => {
            let refusal: Refusal = ipc::decode_payload(payload)?;
            Err(Error::Refused(Refused::from_wire(refusal.reason)))
        }
        other => Err(Error::Protocol(other)),
    }
}

/// A request off the wire, as the compositor reads it: `None` for anything
/// this protocol cannot carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    Get,
    SetMode { width: u32, height: u32 },
    SetScale(Setting),
}

impl Request {
    pub fn decode(msg_type: u32, payload: &[u8]) -> Option<Self> {
        match msg_type {
            MSG_GET if payload.is_empty() => Some(Self::Get),
            MSG_SET_MODE => {
                let req: ModeRequest = ipc::decode_payload(payload).ok()?;
                Some(Self::SetMode { width: req.width, height: req.height })
            }
            MSG_SET_SCALE => {
                let req: ScaleRequest = ipc::decode_payload(payload).ok()?;
                Setting::from_wire(req.scale).map(Self::SetScale)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(words: &[u32]) -> Vec<u8> {
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    #[test]
    fn each_request_decodes() {
        assert_eq!(Request::decode(MSG_GET, &[]), Some(Request::Get));
        assert_eq!(
            Request::decode(MSG_SET_MODE, &bytes(&[1280, 800])),
            Some(Request::SetMode { width: 1280, height: 800 })
        );
        assert_eq!(Request::decode(MSG_SET_SCALE, &bytes(&[0])), Some(Request::SetScale(Setting::Auto)));
        assert_eq!(
            Request::decode(MSG_SET_SCALE, &bytes(&[150])),
            Some(Request::SetScale(Setting::Kept("1.25".parse().unwrap())))
        );
    }

    /// The scale on the wire is held to the protocol's range before anything
    /// reads it as a scale: zero is auto, and 1 to 119 or past 480 is no
    /// request at all.
    #[test]
    fn an_absurd_scale_is_no_request() {
        for absurd in [1, 60, 119, 481, 12_000, u32::MAX] {
            assert_eq!(Request::decode(MSG_SET_SCALE, &bytes(&[absurd])), None, "{absurd}");
        }
    }

    #[test]
    fn a_payload_of_another_length_is_no_request() {
        assert_eq!(Request::decode(MSG_GET, &[0]), None);
        assert_eq!(Request::decode(MSG_SET_MODE, &bytes(&[1280])), None);
        assert_eq!(Request::decode(MSG_SET_MODE, &bytes(&[1280, 800, 0])), None);
        assert_eq!(Request::decode(MSG_SET_SCALE, &bytes(&[120, 0])), None);
        assert_eq!(Request::decode(99, &[]), None);
    }

    #[test]
    fn what_is_shown_crosses_the_wire_whole() {
        let shown = Shown { width: 1920, height: 1080, scale: "1.5".parse().unwrap(), setting: Setting::Auto };
        assert_eq!(Shown::from_wire(shown.wire()), Some(shown));
        let kept = Shown { setting: Setting::Kept(Scale::ONE), scale: Scale::ONE, ..shown };
        assert_eq!(Shown::from_wire(kept.wire()), Some(kept));
        assert_eq!(Shown::from_wire(State { scale: 0, ..shown.wire() }), None);
    }

    #[test]
    fn every_refusal_crosses_the_wire_as_itself() {
        for why in [Refused::NotAStep, Refused::BelowLeast, Refused::NoModes, Refused::OutOfBounds, Refused::Declined] {
            assert_eq!(Refused::from_wire(why.wire()), why);
        }
        assert_eq!(Refused::from_wire(4242), Refused::Other(4242));
    }
}
