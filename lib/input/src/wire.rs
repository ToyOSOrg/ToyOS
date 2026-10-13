//! The `input` port's frames, and the records the kernel's streams carry,
//! decoded by one reader whichever source sent them.
//!
//! Every byte here is one a source chose, and a source is outside the owner's
//! trust: a record that does not decode is refused by name and changes
//! nothing.

use toyos_abi::input::{MouseEvent, RawKeyEvent, MOD_RELEASED, POINTER_ABSOLUTE, POINTER_ABSOLUTE_MAX, POINTER_RELATIVE};

/// The port the owner of input serves.
pub const PORT: &str = "input";

/// The one row that may receive [`PORT`]: a connector is the authority to type
/// into the focused window.
pub const HOLDER: &str = "ps2server";

/// The most connections the owner serves at once; one past it is refused.
pub const CONNECTIONS: usize = 4;

/// A key: one [`RawKeyEvent`].
pub const MSG_KEY: u32 = 1;
/// A pointer report: one [`MouseEvent`].
pub const MSG_POINTER: u32 = 2;
/// No payload: the source's keyboard lost a transition, so every key it
/// holds is released. Its pointers keep their buttons: a report restates them.
pub const MSG_LOST: u32 = 3;

/// A key's bytes on the wire.
pub const KEY_LEN: usize = core::mem::size_of::<RawKeyEvent>();
/// A pointer report's bytes on the wire.
pub const POINTER_LEN: usize = core::mem::size_of::<MouseEvent>();
/// How much of a frame's payload the owner keeps: one byte past the longest
/// record, so a longer frame reads as one and is refused.
pub const KEEP: usize = POINTER_LEN + 1;

/// The buttons boot-mouse order names.
pub const BUTTONS: u8 = 0x1F;

/// Where a pointer report puts the pointer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Motion {
    /// A movement in the device's counts, down the screen positive.
    Relative { dx: i16, dy: i16 },
    /// A position, each axis in `0..=POINTER_ABSOLUTE_MAX`.
    Absolute { x: u16, y: u16 },
}

/// One pointer report, believed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Report {
    /// Which of its sender's pointers this is.
    pub source: u8,
    /// That pointer's own buttons, in boot-mouse order.
    pub buttons: u8,
    pub scroll: i8,
    pub motion: Motion,
}

/// One thing a source said.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Record {
    Key(RawKeyEvent),
    Pointer(Report),
    Lost,
}

/// Why a record was not believed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refused {
    /// A frame type this port does not carry.
    Type(u32),
    /// A payload of the wrong length for its type.
    Length { msg_type: u32, len: usize },
    /// Usage 0, which names no key.
    NoUsage,
    /// A modifier bit beside `MOD_RELEASED`: the mask is the owner's to make.
    Modifiers(u8),
    /// Neither relative nor absolute.
    Kind(u8),
    /// A button past boot-mouse order's five.
    Buttons(u8),
    /// An absolute position off the surface.
    Position { x: i16, y: i16 },
}

impl Report {
    /// This report as its wire record.
    pub fn event(&self) -> MouseEvent {
        let (kind, x, y) = match self.motion {
            Motion::Relative { dx, dy } => (POINTER_RELATIVE, dx, dy),
            // Both are at most `POINTER_ABSOLUTE_MAX`, which an `i16` holds.
            Motion::Absolute { x, y } => (POINTER_ABSOLUTE, x as i16, y as i16),
        };
        MouseEvent { source: self.source, buttons: self.buttons, scroll: self.scroll, kind, x, y }
    }
}

impl Record {
    /// The frame this record is: its type, and its payload in `out`'s first
    /// bytes, whose count is the third word.
    pub fn frame(&self, out: &mut [u8; POINTER_LEN]) -> (u32, usize) {
        let (msg_type, bytes): (u32, &[u8]) = match self {
            Self::Key(key) => (MSG_KEY, toyos_abi::usersafe::bytes(key)),
            Self::Pointer(report) => {
                let event = report.event();
                out.copy_from_slice(toyos_abi::usersafe::bytes(&event));
                return (MSG_POINTER, POINTER_LEN);
            }
            Self::Lost => (MSG_LOST, &[]),
        };
        out[..bytes.len()].copy_from_slice(bytes);
        (msg_type, bytes.len())
    }
}

/// The record a frame of `msg_type` carrying `payload` is.
pub fn decode(msg_type: u32, payload: &[u8]) -> Result<Record, Refused> {
    let length = || Refused::Length { msg_type, len: payload.len() };
    match msg_type {
        MSG_KEY => key(payload.try_into().map_err(|_| length())?).map(Record::Key),
        MSG_POINTER => pointer(payload.try_into().map_err(|_| length())?).map(Record::Pointer),
        MSG_LOST if payload.is_empty() => Ok(Record::Lost),
        MSG_LOST => Err(length()),
        other => Err(Refused::Type(other)),
    }
}

/// One key, as a source's frame or the kernel's keyboard stream carries it.
pub fn key(bytes: &[u8; KEY_LEN]) -> Result<RawKeyEvent, Refused> {
    let [keycode, modifiers] = *bytes;
    if keycode == 0 {
        return Err(Refused::NoUsage);
    }
    if modifiers & !MOD_RELEASED != 0 {
        return Err(Refused::Modifiers(modifiers));
    }
    Ok(RawKeyEvent { keycode, modifiers })
}

/// One pointer report, as a source's frame or the kernel's mouse stream
/// carries it.
pub fn pointer(bytes: &[u8; POINTER_LEN]) -> Result<Report, Refused> {
    let [source, buttons, scroll, kind, x0, x1, y0, y1] = *bytes;
    let (x, y) = (i16::from_ne_bytes([x0, x1]), i16::from_ne_bytes([y0, y1]));
    if buttons & !BUTTONS != 0 {
        return Err(Refused::Buttons(buttons));
    }
    let on_surface = |v: i16| (0..=POINTER_ABSOLUTE_MAX).contains(&v);
    let motion = match kind {
        POINTER_RELATIVE => Motion::Relative { dx: x, dy: y },
        POINTER_ABSOLUTE if on_surface(x) && on_surface(y) => Motion::Absolute { x: x as u16, y: y as u16 },
        POINTER_ABSOLUTE => return Err(Refused::Position { x, y }),
        other => return Err(Refused::Kind(other)),
    };
    Ok(Report { source, buttons, scroll: scroll as i8, motion })
}
