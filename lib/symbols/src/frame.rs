//! A killed program's frame, as the kernel records it and userland reads it
//! back: `    <pc>  <file>+<offset> id=<build-id>`.
//!
//! The kernel names no user frame: it records the file it opened, the offset
//! of the instruction in flight in that file's own addresses, and the build-id
//! the file carries, and [`crate::name`] names the function from them. One
//! encoder ([`UserFrame`]'s `Display`) and one decoder ([`decode`]), so the
//! grammar has no second spelling.
//!
//! - **The offset names a byte of the instruction in flight**: the faulting
//!   one, or for a return address the call's last byte (`pc - 1`), so a call
//!   in tail position names its caller and not the function after it.
//! - **The name is escaped**: every byte below `!`, DEL, `\`, `+` and `[` is
//!   `\xHH`, so the name is one token whatever path the kernel opened, and a
//!   raw `[` in it is only ever the elision marker.
//! - **Only the name is elided** ([`NAME_HEAD`], [`NAME_TAIL`]): offset and id
//!   are always whole, and an elided name is refused by name, never guessed at.
//! - **`id=-`** is a file that carries no build-id.

use core::fmt::{self, Display, Write};

use toyos_abi::log::MAX_RECORD_MESSAGE;
use toyos_elf::note::{self, NoteSegment, MAX_BUILD_ID};
use toyos_elide::{widest, Elided, MARKER_MAX};

/// A user frame's own text, every number at its widest: indent, `0x` and
/// sixteen digits, two spaces, `+`, the offset, ` id=` and the id in hex.
pub const USER_FRAME_TEXT: usize = 4 + 18 + 2 + 1 + 18 + 4 + 2 * MAX_BUILD_ID;
const NAME_KEPT: usize = MAX_RECORD_MESSAGE - USER_FRAME_TEXT - MARKER_MAX;
pub const NAME_HEAD: usize = NAME_KEPT / 2;
pub const NAME_TAIL: usize = NAME_KEPT - NAME_HEAD;
const _: () = assert!(widest(NAME_HEAD, NAME_TAIL) + USER_FRAME_TEXT <= MAX_RECORD_MESSAGE);

/// A build-id: 1 to [`MAX_BUILD_ID`] bytes, checked where it is made.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BuildId {
    bytes: [u8; MAX_BUILD_ID],
    len: u8,
}

impl BuildId {
    pub fn new(id: &[u8]) -> Option<BuildId> {
        let mut bytes = [0; MAX_BUILD_ID];
        bytes.get_mut(..id.len()).filter(|_| !id.is_empty())?.copy_from_slice(id);
        Some(BuildId { bytes, len: id.len() as u8 })
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.bytes.get(..usize::from(self.len)).unwrap_or(&[])
    }

    /// The build-id of the image whose program header table `headers` holds:
    /// the first `PT_NOTE` that `read` hands back bytes for and that carries
    /// one. `read` answers a segment's bytes, as many of them as it will read.
    pub fn find<B: AsRef<[u8]>>(headers: &[u8], mut read: impl FnMut(NoteSegment) -> Option<B>) -> Option<BuildId> {
        note::segments(headers).find_map(|segment| {
            let bytes = read(segment)?;
            note::build_id(bytes.as_ref(), segment.align).and_then(BuildId::new)
        })
    }
}

impl Display for BuildId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_bytes().iter().try_for_each(|b| write!(f, "{b:02x}"))
    }
}

/// `pc`'s offset in the file of an image mapped at `[start, end)`, `bias` above
/// the addresses the file names; `None` when `pc` is not in it.
pub fn frame_offset(pc: u64, start: u64, end: u64, bias: u64) -> Option<u64> {
    if (start..end).contains(&pc) { pc.checked_sub(bias) } else { None }
}

/// One frame of a killed program; its `Display` is the record line.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct UserFrame<'a> {
    pub pc: u64,
    /// The file the kernel opened.
    pub name: &'a str,
    pub offset: u64,
    pub build_id: Option<BuildId>,
}

impl Display for UserFrame<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = Elided::<_, NAME_HEAD, NAME_TAIL>(Escaped(self.name));
        write!(f, "    {:#x}  {}+{:#x} id=", self.pc, name, self.offset)?;
        match &self.build_id {
            Some(id) => id.fmt(f),
            None => f.write_char('-'),
        }
    }
}

struct Escaped<'a>(&'a str);

fn escaped(c: char) -> bool {
    c < '!' || matches!(c, '\x7f' | '\\' | '+' | '[')
}

impl Display for Escaped<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for c in self.0.chars() {
            if escaped(c) {
                write!(f, "\\x{:02x}", u32::from(c))?;
            } else {
                f.write_char(c)?;
            }
        }
        Ok(())
    }
}

/// Why a line shaped like a frame names nothing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refused {
    /// The record elided the middle of the name, so it names no file.
    Elided,
    /// A `\` in the name that is not `\x` and two hex digits of an ASCII byte.
    BadEscape,
    /// The id is neither `-` nor 1 to 32 bytes of lowercase hex.
    BadId,
}

impl Refused {
    pub const fn as_str(self) -> &'static str {
        match self {
            Refused::Elided => "the record elided the file's name",
            Refused::BadEscape => "the file's name holds an escape no encoder writes",
            Refused::BadId => "the build-id is not 1 to 32 bytes of lowercase hex",
        }
    }
}

/// A frame read back out of a line.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Decoded<'a> {
    pub pc: u64,
    escaped_name: &'a str,
    pub offset: u64,
    pub build_id: Option<BuildId>,
}

impl<'a> Decoded<'a> {
    /// The file's name, unescaped.
    pub fn name(&self) -> impl Iterator<Item = char> + 'a {
        let mut chars = self.escaped_name.chars();
        core::iter::from_fn(move || match chars.next()? {
            '\\' => {
                let hex = [chars.next()?, chars.next()?, chars.next()?];
                let byte = hex_byte(hex[1], hex[2])?;
                Some(char::from(byte))
            }
            c => Some(c),
        })
    }
}

/// The frame `line` records, wherever in the line it starts: `None` when the
/// line is not shaped like one, and the refusal when it is and names nothing.
pub fn decode(line: &str) -> Option<Result<Decoded<'_>, Refused>> {
    let line = line.trim_end();
    let (head, id) = line.rsplit_once(" id=")?;
    let (head, offset) = head.rsplit_once("+0x")?;
    let offset = hex_u64(offset)?;
    let (head, escaped_name) = head.rsplit_once("  ")?;
    let pc = head.rsplit(' ').next().and_then(|pc| pc.strip_prefix("0x")).and_then(hex_u64)?;
    Some(decoded(pc, escaped_name, offset, id))
}

fn decoded<'a>(pc: u64, escaped_name: &'a str, offset: u64, id: &str) -> Result<Decoded<'a>, Refused> {
    if escaped_name.contains('[') {
        return Err(Refused::Elided);
    }
    let mut rest = escaped_name;
    while let Some((_, after)) = rest.split_once('\\') {
        let mut c = after.chars();
        match (c.next(), c.next(), c.next()) {
            (Some('x'), Some(hi), Some(lo)) if hex_byte(hi, lo).is_some() => rest = c.as_str(),
            _ => return Err(Refused::BadEscape),
        }
    }
    let build_id = match id {
        "-" => None,
        id => Some(build_id(id).ok_or(Refused::BadId)?),
    };
    Ok(Decoded { pc, escaped_name, offset, build_id })
}

fn build_id(hex: &str) -> Option<BuildId> {
    let mut bytes = [0u8; MAX_BUILD_ID];
    let mut len = 0usize;
    let mut digits = hex.chars();
    while let Some(hi) = digits.next() {
        *bytes.get_mut(len)? = hex_byte_any(hi, digits.next()?)?;
        len += 1;
    }
    BuildId::new(bytes.get(..len)?)
}

/// A lowercase hex digit's value.
fn digit(c: char) -> Option<u8> {
    match c {
        '0'..='9' | 'a'..='f' => c.to_digit(16).map(|d| d as u8),
        _ => None,
    }
}

fn hex_byte_any(hi: char, lo: char) -> Option<u8> {
    Some(digit(hi)? << 4 | digit(lo)?)
}

/// An escaped byte: ASCII, since a name is UTF-8 and the encoder escapes only
/// ASCII characters.
fn hex_byte(hi: char, lo: char) -> Option<u8> {
    hex_byte_any(hi, lo).filter(u8::is_ascii)
}

/// 1 to 16 lowercase hex digits.
fn hex_u64(s: &str) -> Option<u64> {
    if s.is_empty() || s.len() > 16 {
        return None;
    }
    s.chars().try_fold(0u64, |n, c| Some(n << 4 | u64::from(digit(c)?)))
}
