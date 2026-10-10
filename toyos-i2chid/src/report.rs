//! A report descriptor's mouse collection (HID 1.11 §6.2.2), and its input
//! reports read against it.
//!
//! A precision touchpad starts in mouse mode (Microsoft's Windows Precision
//! Touchpad device guidance: it reports through its mouse collection until
//! the host sets the input mode feature), so the application collection of
//! Generic Desktop's Mouse is the one read; every other collection's items
//! only advance the bit offsets of the report ids they name.

/// Generic Desktop (HID Usage Tables §4).
const GENERIC_DESKTOP: u16 = 0x01;
const BUTTON: u16 = 0x09;
const MOUSE: u16 = 0x02;
const X: u16 = 0x30;
const Y: u16 = 0x31;
const WHEEL: u16 = 0x38;

/// The most buttons one report carries into the kernel's byte of buttons.
pub const BUTTONS: usize = 8;
/// The most usages one main item names by Usage alone.
const USAGES: usize = 16;
/// The longest input report this reader lays out: the most an input
/// register's 16-bit length field can carry.
pub const MAX_REPORT_BITS: u64 = 0xFFFF * 8;
/// The deepest collection nesting this reader follows.
const DEPTH: usize = 16;

/// One field: where in a report (the report id byte not counted) and how
/// wide, and whether its value is two's complement.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Field {
    pub bit: u32,
    pub size: u8,
    pub signed: bool,
}

/// Where the mouse collection's input report puts what it moved.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Mouse {
    /// The report id its input report starts with, or `None` for a
    /// descriptor that declares none.
    pub report_id: Option<u8>,
    /// Button `n + 1`'s bit, where the collection has one.
    pub buttons: [Option<u32>; BUTTONS],
    pub x: Field,
    pub y: Field,
    pub wheel: Option<Field>,
    /// The report's length in bytes past its id.
    pub len: usize,
}

/// Why a report descriptor yielded no mouse.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refused {
    /// An item whose data runs past the descriptor, at this offset.
    Truncated(usize),
    /// An End Collection with no collection open, at this offset.
    Unbalanced(usize),
    /// Collections nested past [`DEPTH`], at this offset.
    TooDeep(usize),
    /// An input report past [`MAX_REPORT_BITS`], at this offset.
    TooLong(usize),
    /// A Report Size of 0 or past 32 on a mouse field, at this offset.
    FieldSize(usize),
    /// The mouse collection's fields span two report ids.
    TwoReports,
    /// A mouse collection whose X or Y is absolute: this reader moves the
    /// pointer by relative motion only.
    Absolute,
    /// No Generic Desktop Mouse application collection with both X and Y.
    NoMouse,
}

#[derive(Clone, Copy, Default)]
struct Globals {
    page: u16,
    logical_min: i32,
    size: u32,
    count: u32,
    report_id: u8,
}

/// Find the mouse collection's input report in `descriptor`.
pub fn mouse(descriptor: &[u8]) -> Result<Mouse, Refused> {
    let mut g = Globals::default();
    let mut stack = [Globals::default(); 4];
    let mut pushed = 0usize;
    let mut usages = [0u32; USAGES];
    let mut n_usages = 0usize;
    let (mut usage_min, mut usage_max) = (None::<u32>, None::<u32>);
    // Input bits laid out so far, per report id.
    let mut offset = [0u32; 256];
    let mut any_id = false;
    let mut depth = 0usize;
    // The depth the mouse application collection opened at, while it is open.
    let mut inside: Option<usize> = None;
    let mut found: Option<Mouse> = None;
    let mut cur = Partial::default();

    let mut at = 0usize;
    while at < descriptor.len() {
        let prefix = descriptor[at];
        if prefix == 0xFE {
            // A long item (§6.2.2.3): its size, its tag, its data; none is defined.
            let Some(&len) = descriptor.get(at + 1) else { return Err(Refused::Truncated(at)) };
            at += 3 + len as usize;
            if at > descriptor.len() {
                return Err(Refused::Truncated(at));
            }
            continue;
        }
        let size = [0usize, 1, 2, 4][(prefix & 3) as usize];
        let Some(data) = descriptor.get(at + 1..at + 1 + size) else { return Err(Refused::Truncated(at)) };
        let unsigned = data.iter().rev().fold(0u32, |v, &b| v << 8 | b as u32);
        let signed = match size {
            1 => data[0] as i8 as i32,
            2 => i16::from_le_bytes([data[0], data[1]]) as i32,
            4 => unsigned as i32,
            _ => 0,
        };
        let item = at;
        at += 1 + size;
        match (prefix >> 2) & 3 {
            // Main.
            0 => {
                let tag = prefix >> 4;
                match tag {
                    // Input.
                    0x8 => {
                        let start = offset[g.report_id as usize];
                        let end = start as u64 + g.size as u64 * g.count as u64;
                        if end > MAX_REPORT_BITS {
                            return Err(Refused::TooLong(item));
                        }
                        if inside.is_some() && unsigned & 1 == 0 && unsigned & 2 != 0 {
                            if g.size == 0 || g.size > 32 {
                                return Err(Refused::FieldSize(item));
                            }
                            for i in 0..g.count.min(64) {
                                let usage = if i < n_usages as u32 {
                                    usages[i as usize]
                                } else if let (Some(lo), Some(hi)) = (usage_min, usage_max) {
                                    match lo.checked_add(i) {
                                        Some(u) if u <= hi => u,
                                        _ => continue,
                                    }
                                } else if n_usages > 0 {
                                    usages[n_usages - 1]
                                } else {
                                    continue;
                                };
                                let (page, id) = match usage >> 16 {
                                    0 => (g.page, usage as u16),
                                    p => (p as u16, usage as u16),
                                };
                                let field = Field {
                                    bit: start + i * g.size,
                                    size: g.size as u8,
                                    signed: g.logical_min < 0,
                                };
                                cur.take(page, id, field, unsigned & 4 != 0, g.report_id)?;
                            }
                        }
                        offset[g.report_id as usize] = end as u32;
                    }
                    // Collection.
                    0xA => {
                        if depth == DEPTH {
                            return Err(Refused::TooDeep(item));
                        }
                        let usage = if n_usages > 0 { usages[0] } else { usage_min.unwrap_or(0) };
                        let page = match usage >> 16 {
                            0 => g.page,
                            p => p as u16,
                        };
                        // Application (§6.2.2.6: data 1).
                        if inside.is_none() && found.is_none() && unsigned == 1 && page == GENERIC_DESKTOP && usage as u16 == MOUSE {
                            inside = Some(depth);
                            cur = Partial::default();
                        }
                        depth += 1;
                    }
                    // End Collection.
                    0xC => {
                        if depth == 0 {
                            return Err(Refused::Unbalanced(item));
                        }
                        depth -= 1;
                        if inside == Some(depth) {
                            inside = None;
                            found = Some(cur.finish(any_id, &offset)?);
                        }
                    }
                    _ => {}
                }
                n_usages = 0;
                usage_min = None;
                usage_max = None;
            }
            // Global.
            1 => match prefix >> 4 {
                0x0 => g.page = unsigned as u16,
                0x1 => g.logical_min = signed,
                0x7 => g.size = unsigned,
                0x8 => {
                    g.report_id = unsigned as u8;
                    any_id = true;
                }
                0x9 => g.count = unsigned,
                0xA if pushed < stack.len() => {
                    stack[pushed] = g;
                    pushed += 1;
                }
                0xB if pushed > 0 => {
                    pushed -= 1;
                    g = stack[pushed];
                }
                _ => {}
            },
            // Local: a 4-byte usage carries its own page in its high half.
            2 => {
                let usage = if size == 4 { unsigned } else { unsigned & 0xFFFF };
                match prefix >> 4 {
                    0x0 => {
                        if n_usages < USAGES {
                            usages[n_usages] = usage;
                            n_usages += 1;
                        }
                    }
                    0x1 => usage_min = Some(usage),
                    0x2 => usage_max = Some(usage),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    found.ok_or(Refused::NoMouse)
}

/// The mouse collection as its items arrive.
#[derive(Default)]
struct Partial {
    report_id: Option<u8>,
    buttons: [Option<u32>; BUTTONS],
    x: Option<Field>,
    y: Option<Field>,
    wheel: Option<Field>,
    absolute: bool,
}

impl Partial {
    fn take(&mut self, page: u16, usage: u16, field: Field, relative: bool, id: u8) -> Result<(), Refused> {
        let slot = match (page, usage) {
            (BUTTON, 1..=8) if field.size == 1 => {
                self.buttons[usage as usize - 1] = Some(field.bit);
                None
            }
            (GENERIC_DESKTOP, X) => Some(&mut self.x),
            (GENERIC_DESKTOP, Y) => Some(&mut self.y),
            (GENERIC_DESKTOP, WHEEL) => Some(&mut self.wheel),
            _ => return Ok(()),
        };
        match self.report_id {
            Some(seen) if seen != id => return Err(Refused::TwoReports),
            _ => self.report_id = Some(id),
        }
        if let Some(slot) = slot {
            if !relative && usage != WHEEL {
                self.absolute = true;
            }
            *slot = Some(field);
        }
        Ok(())
    }

    fn finish(&self, any_id: bool, offset: &[u32; 256]) -> Result<Mouse, Refused> {
        let (Some(x), Some(y), Some(id)) = (self.x, self.y, self.report_id) else { return Err(Refused::NoMouse) };
        if self.absolute {
            return Err(Refused::Absolute);
        }
        Ok(Mouse {
            report_id: any_id.then_some(id),
            buttons: self.buttons,
            x,
            y,
            wheel: self.wheel,
            len: (offset[id as usize] as usize).div_ceil(8),
        })
    }
}

/// One mouse report, read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Motion {
    pub buttons: u8,
    pub dx: i32,
    pub dy: i32,
    pub wheel: i32,
}

/// Why a report was not read as the mouse's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unread {
    /// Another collection's report: its id.
    OtherReport(u8),
    /// Shorter than the mouse's layout.
    Short(usize),
}

impl Mouse {
    /// `report` (its id first, where the descriptor declares ids) as this
    /// layout reads it.
    pub fn read(&self, report: &[u8]) -> Result<Motion, Unread> {
        let body = match self.report_id {
            Some(id) => match report.split_first() {
                Some((&got, body)) if got == id => body,
                Some((&got, _)) => return Err(Unread::OtherReport(got)),
                None => return Err(Unread::Short(0)),
            },
            None => report,
        };
        if body.len() < self.len {
            return Err(Unread::Short(report.len()));
        }
        let mut buttons = 0u8;
        for (n, bit) in self.buttons.iter().enumerate() {
            if let Some(bit) = bit {
                buttons |= (extract(body, *bit, 1) as u8) << n;
            }
        }
        Ok(Motion {
            buttons,
            dx: value(body, self.x),
            dy: value(body, self.y),
            wheel: self.wheel.map_or(0, |w| value(body, w)),
        })
    }

    /// Bytes an input-register read takes to carry this report whole: the
    /// length field, the id and the body.
    pub fn read_len(&self) -> usize {
        2 + self.report_id.is_some() as usize + self.len
    }
}

fn value(body: &[u8], f: Field) -> i32 {
    let raw = extract(body, f.bit, f.size);
    if f.signed && f.size < 32 && raw >> (f.size - 1) & 1 != 0 {
        (raw | !0u32 << f.size) as i32
    } else {
        raw as i32
    }
}

/// `size` bits at `bit`, little endian; bits past `body` read as zero.
fn extract(body: &[u8], bit: u32, size: u8) -> u32 {
    let mut v = 0u64;
    for k in 0..size as u32 {
        let at = bit as u64 + k as u64;
        let byte = body.get((at / 8) as usize).copied().unwrap_or(0);
        v |= ((byte >> (at % 8)) as u64 & 1) << k;
    }
    v as u32
}
