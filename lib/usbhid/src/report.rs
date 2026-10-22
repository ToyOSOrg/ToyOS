//! One interface's input reports, read through its report descriptor into
//! key transitions and pointer reports for each bound collection.
//!
//! **A key field is read as HID 1.11 and the HID Usage Tables 1.5 define it.**
//! - An array field reports indexes (§6.2.2.5): a value outside Logical
//!   Minimum to Maximum asserts nothing, and one inside names the usage at
//!   index `value - Logical Minimum` of the field's list; an index past the
//!   list names nothing and is counted [`Counts::unlisted`].
//! - A variable field is a bitmap: a control is held at 1 and released at 0
//!   (§5.9), and a value outside its logical range leaves it as it was (§5.10).
//! - Usages `00` to `03` are never keys: `00` is an empty slot, and `01` to
//!   `03` are "sent as a member of the keyboard array. Not a physical key"
//!   (HUT 1.5 p.96, note 1). In an array, ErrorRollOver (`01`) is Appendix C's
//!   phantom state, in which the report's variable fields are believed and its
//!   arrays are not, so the keys an array held stay held; POSTFail (`02`) and
//!   ErrorUndefined (`03`) refuse the report.
//! - A usage HUT 1.5 reserves — `A5-AF` (p.94), `DE-DF` (p.95) and `E8-FFFF`
//!   (p.96) — names no key and is counted [`Counts::reserved`].
//!
//! **A report says only what its own fields cover.** A keyboard whose keys
//! come in two reports releases, on each, only the usages that report's
//! fields can name.
//!
//! **Every pointer field is bounded to the `input` record** (`toyos-input`'s
//! wire): the Button page's usages 1 to 5 are its five buttons, and one past
//! them pressed is counted [`Counts::unsent_button`]; relative motion and the
//! wheel saturate; an absolute axis is scaled from its logical range onto
//! `0..=POINTER_ABSOLUTE_MAX`, and a value outside that range leaves the
//! position as it was (§5.10).

use toyos_abi::input::POINTER_ABSOLUTE_MAX;
use toyos_input::keys::{Keys, Transitions, Usages};
use toyos_input::wire::{Motion, BUTTONS};

use crate::descriptor::{Collection, Field, Interface, Kind, BUTTON, GENERIC_DESKTOP, KEYS, WHEEL, X, Y};
use alloc::vec::Vec;

const ERROR_ROLL_OVER: u16 = 0x01;
const POST_FAIL: u16 = 0x02;
const ERROR_UNDEFINED: u16 = 0x03;

/// Why a report was not believed; nothing it carried moved anything.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refused {
    /// An empty transfer: an interface that numbers its reports sent no ID.
    Empty,
    /// A Report ID no collection's fields carry.
    UnknownReport(u8),
    /// Fewer bytes than the report's fields need.
    Short { id: u8, len: usize },
    /// POSTFail in a key array: the keyboard failed its own power-on test.
    PostFail,
    /// ErrorUndefined in a key array: an error the keyboard does not name.
    Undefined,
}

/// What one believed report did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Read {
    /// Collection `collection`, a keyboard, made these transitions; `rolled`
    /// where an array reported ErrorRollOver and only its variable fields
    /// were believed.
    Keys { collection: usize, made: Transitions, rolled: bool },
    /// Collection `collection`, a pointer, reported this.
    Pointer { collection: usize, report: Pointer },
    /// The report is collection `collection`'s, which is not bound, and was
    /// dropped.
    Unbound { collection: usize },
}

/// A pointer report, bounded to the `input` record.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Pointer {
    /// Boot-mouse order's five.
    pub buttons: u8,
    pub scroll: i8,
    pub motion: Motion,
}

/// What reports carried that was not delivered, for `inspect`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Counts {
    pub reserved: u64,
    pub unlisted: u64,
    pub unsent_button: u64,
    pub unbound: u64,
}

/// One collection's state between its reports.
#[derive(Clone, PartialEq, Eq, Debug)]
enum State {
    Keyboard(Keys),
    /// The last absolute position each axis was told, which a value outside
    /// its range leaves as it was.
    Pointer { x: u16, y: u16 },
    Unbound,
}

/// One HID interface, bound by its descriptor.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Hid {
    interface: Interface,
    states: Vec<State>,
    pub counts: Counts,
}

impl Hid {
    pub fn new(interface: Interface) -> Self {
        let states = interface
            .collections
            .iter()
            .map(|c| match c.bound {
                Ok(Kind::Keyboard) => State::Keyboard(Keys::default()),
                Ok(Kind::Pointer) => State::Pointer { x: 0, y: 0 },
                Err(_) => State::Unbound,
            })
            .collect();
        Self { interface, states, counts: Counts::default() }
    }

    pub fn collections(&self) -> &[Collection] {
        &self.interface.collections
    }

    /// The transitions and reports `report`, one interrupt transfer, makes.
    pub fn report(&mut self, report: &[u8]) -> Result<Read, Refused> {
        let (id, body) = match self.interface.numbered {
            true => report.split_first().map(|(id, body)| (*id, body)).ok_or(Refused::Empty)?,
            false => (0, report),
        };
        let collection = self.interface.of(id).ok_or(Refused::UnknownReport(id))?;
        if self.states[collection] == State::Unbound {
            self.counts.unbound += 1;
            return Ok(Read::Unbound { collection });
        }
        let fields: Vec<&Field> = self.interface.collections[collection].fields.iter().filter(|f| f.report == id).collect();
        if fields.iter().any(|f| f.end() > body.len() as u64 * 8) {
            return Err(Refused::Short { id, len: report.len() });
        }
        match &mut self.states[collection] {
            State::Unbound => unreachable!("answered above"),
            State::Keyboard(keys) => {
                let (next, rolled) = read_keys(&fields, body, keys.held(), &mut self.counts)?;
                Ok(Read::Keys { collection, made: keys.hold(next), rolled })
            }
            State::Pointer { x, y } => {
                let report = read_pointer(&fields, body, (x, y), &mut self.counts);
                Ok(Read::Pointer { collection, report })
            }
        }
    }

    /// The keys keyboard collection `collection` holds.
    pub fn held(&self, collection: usize) -> Option<Usages> {
        match &self.states[collection] {
            State::Keyboard(keys) => Some(keys.held()),
            _ => None,
        }
    }

    /// Collection `collection`'s keys released: the device left.
    pub fn release(&mut self, collection: usize) -> Option<Transitions> {
        match &mut self.states[collection] {
            State::Keyboard(keys) => Some(keys.hold(Usages::NONE)),
            _ => None,
        }
    }
}

/// What a key usage is to the owner of input.
enum Key {
    Held(u8),
    /// `00`, or a bitmap's `01` to `03`: nothing.
    Nothing,
    Reserved,
}

fn key(usage: u16) -> Key {
    match usage {
        0x00..=0x03 => Key::Nothing,
        0xA5..=0xAF | 0xDE..=0xDF | 0xE8..=0xFFFF => Key::Reserved,
        usage => Key::Held(usage as u8),
    }
}

/// The keys `body` holds, from `held` before it, and whether an array rolled
/// over.
fn read_keys(fields: &[&Field], body: &[u8], held: Usages, counts: &mut Counts) -> Result<(Usages, bool), Refused> {
    let (mut covered, mut down) = (Usages::NONE, Usages::NONE);
    let (mut array_covered, mut array_down) = (Usages::NONE, Usages::NONE);
    let mut rolled = false;
    let mut reserved = 0;
    let mut unlisted = 0;
    for field in fields.iter().filter(|f| f.data && f.keys()) {
        if field.variable {
            for i in 0..field.count {
                let (Some((KEYS, usage)), Some(value)) = (field.control(i), field.value(body, i)) else { continue };
                match key(usage) {
                    Key::Held(usage) if field.holds(value) => {
                        covered.insert(usage);
                        if value != 0 {
                            down.insert(usage);
                        }
                    }
                    Key::Reserved if value != 0 && field.holds(value) => reserved += 1,
                    _ => {}
                }
            }
            continue;
        }
        // Every usage the array can name is released unless one of its
        // controls names it now.
        for index in 0..=field.logical_max.saturating_sub(field.logical_min).clamp(0, 0xFF) {
            if let Some(Some((KEYS, usage))) = field.selected(field.logical_min.saturating_add(index)) {
                if let Key::Held(usage) = key(usage) {
                    array_covered.insert(usage);
                }
            }
        }
        for i in 0..field.count {
            let Some(value) = field.value(body, i) else { continue };
            match field.selected(value) {
                None => {}
                Some(None) => unlisted += 1,
                Some(Some((KEYS, ERROR_ROLL_OVER))) => rolled = true,
                Some(Some((KEYS, POST_FAIL))) => return Err(Refused::PostFail),
                Some(Some((KEYS, ERROR_UNDEFINED))) => return Err(Refused::Undefined),
                Some(Some((KEYS, usage))) => match key(usage) {
                    Key::Held(usage) => array_down.insert(usage),
                    Key::Reserved => reserved += 1,
                    Key::Nothing => {}
                },
                // A usage of another page in a keyboard's array is no key.
                Some(Some(_)) => {}
            }
        }
    }
    counts.reserved += reserved;
    counts.unlisted += unlisted;
    if !rolled {
        covered = covered.union(array_covered);
        down = down.union(array_down);
    }
    Ok((held.without(covered).union(down), rolled))
}

/// What `body` reports of a pointer whose last absolute position is `at`.
fn read_pointer(fields: &[&Field], body: &[u8], at: (&mut u16, &mut u16), counts: &mut Counts) -> Pointer {
    let (x_at, y_at) = at;
    let (mut buttons, mut scroll, mut dx, mut dy) = (0u8, 0i32, 0i32, 0i32);
    let mut absolute = false;
    for field in fields.iter().filter(|f| f.data) {
        for i in 0..field.count {
            let Some(value) = field.value(body, i) else { continue };
            let usage = if field.variable {
                if !field.holds(value) {
                    continue;
                }
                field.control(i)
            } else {
                // An array of buttons names the one pressed.
                match field.selected(value) {
                    Some(Some((BUTTON, usage))) => {
                        press(&mut buttons, usage, counts);
                        continue;
                    }
                    _ => continue,
                }
            };
            match usage {
                Some((BUTTON, usage)) if value != 0 => press(&mut buttons, usage, counts),
                Some((GENERIC_DESKTOP, X)) if field.relative => dx = dx.saturating_add(value),
                Some((GENERIC_DESKTOP, Y)) if field.relative => dy = dy.saturating_add(value),
                Some((GENERIC_DESKTOP, X)) => {
                    absolute = true;
                    *x_at = scaled(field, value);
                }
                Some((GENERIC_DESKTOP, Y)) => {
                    absolute = true;
                    *y_at = scaled(field, value);
                }
                Some((GENERIC_DESKTOP, WHEEL)) => scroll = scroll.saturating_add(value),
                _ => {}
            }
        }
    }
    let saturate = |v: i32| v.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
    let motion = match absolute || fields.iter().any(|f| f.data && !f.relative && axis(f)) {
        true => Motion::Absolute { x: *x_at, y: *y_at },
        false => Motion::Relative { dx: saturate(dx), dy: saturate(dy) },
    };
    Pointer { buttons, scroll: scroll.clamp(i32::from(i8::MIN), i32::from(i8::MAX)) as i8, motion }
}

fn axis(field: &Field) -> bool {
    (0..field.count).any(|i| matches!(field.control(i), Some((GENERIC_DESKTOP, X | Y))))
}

fn press(buttons: &mut u8, usage: u16, counts: &mut Counts) {
    match usage {
        1..=5 => *buttons |= 1 << (usage - 1) & BUTTONS,
        0 => {}
        _ => counts.unsent_button += 1,
    }
}

/// `value`, inside `field`'s logical range, scaled onto
/// `0..=POINTER_ABSOLUTE_MAX`, rounded down.
fn scaled(field: &Field, value: i32) -> u16 {
    let span = i64::from(field.logical_max) - i64::from(field.logical_min);
    let from = i64::from(value) - i64::from(field.logical_min);
    (from * i64::from(POINTER_ABSOLUTE_MAX) / span) as u16
}
