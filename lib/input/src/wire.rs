//! The `input` port's frames, and the records the kernel's streams carry,
//! decoded by one reader whichever source sent them.
//!
//! A sender numbers its devices `source`s and never reuses one for the life
//! of its connection, so a late record never reaches a later device.
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

/// A key: [`KEY_LEN`] bytes, the device's `source` little endian, the usage,
/// and [`MOD_RELEASED`] or 0.
pub const MSG_KEY: u32 = 1;
/// A pointer report: [`POINTER_LEN`] bytes, the device's `source` little
/// endian, then the buttons, the wheel, the kind and the two axes little endian.
pub const MSG_POINTER: u32 = 2;
/// [`GONE_LEN`] bytes, a `source` little endian: that device left, or lost a
/// transition, and everything it holds is released. The source's other
/// devices keep what they hold.
pub const MSG_GONE: u32 = 3;

pub const KEY_LEN: usize = 4;
pub const POINTER_LEN: usize = 9;
pub const GONE_LEN: usize = 2;
/// The longest record's payload: the owner refuses a frame declaring more.
pub const MAX_PAYLOAD: usize = POINTER_LEN;

/// The devices one connection may hold anything at once: a record naming a
/// further `source` while this many hold something is the sender breaking the
/// protocol, and its connection is dropped. A sender binds no more devices
/// than this.
pub const DEVICES: usize = 16;

/// A key record's bytes on the kernel's keyboard stream: one [`RawKeyEvent`].
pub const STREAM_KEY_LEN: usize = core::mem::size_of::<RawKeyEvent>();
/// A pointer record's bytes on the kernel's mouse stream: one [`MouseEvent`].
pub const STREAM_POINTER_LEN: usize = core::mem::size_of::<MouseEvent>();

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
    /// Which of its sender's devices this is.
    pub source: u16,
    /// That pointer's own buttons, in boot-mouse order.
    pub buttons: u8,
    pub scroll: i8,
    pub motion: Motion,
}

/// One thing a source said.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Record {
    /// A transition of `source`'s keyboard; its modifiers are
    /// [`MOD_RELEASED`] or nothing.
    Key { source: u16, key: RawKeyEvent },
    Pointer(Report),
    Gone { source: u16 },
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

impl Record {
    /// The frame this record is: its type, and its payload in `out`'s first
    /// bytes, whose count is the third word.
    pub fn frame(&self, out: &mut [u8; MAX_PAYLOAD]) -> (u32, usize) {
        match *self {
            Self::Key { source, key } => {
                let [a, b] = source.to_le_bytes();
                out[..KEY_LEN].copy_from_slice(&[a, b, key.keycode, key.modifiers]);
                (MSG_KEY, KEY_LEN)
            }
            Self::Pointer(Report { source, buttons, scroll, motion }) => {
                let (kind, x, y) = match motion {
                    Motion::Relative { dx, dy } => (POINTER_RELATIVE, dx, dy),
                    // Both are at most `POINTER_ABSOLUTE_MAX`, which an `i16` holds.
                    Motion::Absolute { x, y } => (POINTER_ABSOLUTE, x as i16, y as i16),
                };
                let ([s0, s1], [x0, x1], [y0, y1]) = (source.to_le_bytes(), x.to_le_bytes(), y.to_le_bytes());
                *out = [s0, s1, buttons, scroll as u8, kind, x0, x1, y0, y1];
                (MSG_POINTER, POINTER_LEN)
            }
            Self::Gone { source } => {
                out[..GONE_LEN].copy_from_slice(&source.to_le_bytes());
                (MSG_GONE, GONE_LEN)
            }
        }
    }
}

/// The record a frame of `msg_type` carrying `payload` is.
pub fn decode(msg_type: u32, payload: &[u8]) -> Result<Record, Refused> {
    let length = || Refused::Length { msg_type, len: payload.len() };
    match msg_type {
        MSG_KEY => {
            let [s0, s1, keycode, modifiers]: [u8; KEY_LEN] = payload.try_into().map_err(|_| length())?;
            Ok(Record::Key { source: u16::from_le_bytes([s0, s1]), key: key(&[keycode, modifiers])? })
        }
        MSG_POINTER => {
            let [s0, s1, buttons, scroll, kind, x0, x1, y0, y1]: [u8; POINTER_LEN] =
                payload.try_into().map_err(|_| length())?;
            let motion = motion(kind, i16::from_le_bytes([x0, x1]), i16::from_le_bytes([y0, y1]))?;
            let report = Report { source: u16::from_le_bytes([s0, s1]), buttons, scroll: scroll as i8, motion };
            believed(report).map(Record::Pointer)
        }
        MSG_GONE => {
            let source: [u8; GONE_LEN] = payload.try_into().map_err(|_| length())?;
            Ok(Record::Gone { source: u16::from_le_bytes(source) })
        }
        other => Err(Refused::Type(other)),
    }
}

/// One key's usage and transition, as a source's frame or the kernel's
/// keyboard stream carries it.
pub fn key(bytes: &[u8; STREAM_KEY_LEN]) -> Result<RawKeyEvent, Refused> {
    let [keycode, modifiers] = *bytes;
    if keycode == 0 {
        return Err(Refused::NoUsage);
    }
    if modifiers & !MOD_RELEASED != 0 {
        return Err(Refused::Modifiers(modifiers));
    }
    Ok(RawKeyEvent { keycode, modifiers })
}

/// One pointer report, as the kernel's mouse stream carries it.
pub fn pointer(bytes: &[u8; STREAM_POINTER_LEN]) -> Result<Report, Refused> {
    let [source, buttons, scroll, kind, x0, x1, y0, y1] = *bytes;
    let motion = motion(kind, i16::from_ne_bytes([x0, x1]), i16::from_ne_bytes([y0, y1]))?;
    believed(Report { source: u16::from(source), buttons, scroll: scroll as i8, motion })
}

fn motion(kind: u8, x: i16, y: i16) -> Result<Motion, Refused> {
    let on_surface = |v: i16| (0..=POINTER_ABSOLUTE_MAX).contains(&v);
    match kind {
        POINTER_RELATIVE => Ok(Motion::Relative { dx: x, dy: y }),
        POINTER_ABSOLUTE if on_surface(x) && on_surface(y) => Ok(Motion::Absolute { x: x as u16, y: y as u16 }),
        POINTER_ABSOLUTE => Err(Refused::Position { x, y }),
        other => Err(Refused::Kind(other)),
    }
}

fn believed(report: Report) -> Result<Report, Refused> {
    match report.buttons & !BUTTONS {
        0 => Ok(report),
        _ => Err(Refused::Buttons(report.buttons)),
    }
}
