//! The owner of input's merge: every source's keys into one keyboard, and
//! every pointer's buttons and motion into one pointer.

use toyos_abi::input::{RawKeyEvent, MOD_ALT, MOD_CTRL, MOD_GUI, MOD_RELEASED, MOD_SHIFT, POINTER_ABSOLUTE_MAX};

use crate::keys::{Keys, Transitions, Usages};
use crate::wire::{Motion, Report, CONNECTIONS, DEVICES};

/// The sources the owner merges: one per connection on the port.
pub const SLOTS: usize = CONNECTIONS;

/// Which source a record came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Slot(usize);

impl Slot {
    /// The `n`th connection on the port, of [`CONNECTIONS`].
    pub fn connection(n: usize) -> Option<Self> {
        (n < CONNECTIONS).then_some(Self(n))
    }
}

/// A connection's [`DEVICES`]th device holding something already, and a
/// record for another: the sender broke the protocol.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Full {
    pub source: u16,
}

/// What each device of one slot holds, by its `source`. A device holding
/// nothing has no entry.
struct Devices<T> {
    held: [Option<(u16, T)>; DEVICES],
}

impl<T: Default> Devices<T> {
    const EMPTY: Self = Self { held: [const { None }; DEVICES] };

    /// `source`'s state, with `change` made to it; an entry left holding
    /// nothing, as `empty` judges, goes.
    fn change<R>(&mut self, source: u16, empty: fn(&T) -> bool, change: impl FnOnce(&mut T) -> R) -> Result<R, Full> {
        let at = match self.held.iter().position(|d| matches!(d, Some((s, _)) if *s == source)) {
            Some(at) => at,
            None => {
                let at = self.held.iter().position(Option::is_none).ok_or(Full { source })?;
                self.held[at] = Some((source, T::default()));
                at
            }
        };
        let (_, state) = self.held[at].as_mut().expect("just found or made");
        let changed = change(state);
        if empty(state) {
            self.held[at] = None;
        }
        Ok(changed)
    }

    fn gone(&mut self, source: u16) {
        self.held.iter_mut().filter(|d| matches!(d, Some((s, _)) if *s == source)).for_each(|d| *d = None);
    }

    fn end(&mut self) {
        self.held = [const { None }; DEVICES];
    }

    fn states(&self) -> impl Iterator<Item = &T> {
        self.held.iter().flatten().map(|(_, state)| state)
    }
}

/// One keyboard made of every device's: a key is held while any device
/// holds it.
pub struct Merge {
    slots: [Devices<Keys>; SLOTS],
    union: Keys,
}

impl Default for Merge {
    fn default() -> Self {
        Self { slots: [const { Devices::EMPTY }; SLOTS], union: Keys::default() }
    }
}

impl Merge {
    /// `key` from `slot`'s device `source`: what it changed of the union,
    /// each transition with the union's modifiers after it. A repeated make,
    /// or a key another device already holds, changes nothing.
    pub fn key(&mut self, slot: Slot, source: u16, key: RawKeyEvent) -> Result<Delivered, Full> {
        let empty = |keys: &Keys| keys.held() == Usages::NONE;
        self.slots[slot.0].change(source, empty, |keys| {
            let mut now = keys.held();
            if key.pressed() {
                now.insert(key.keycode);
            } else {
                now.remove(key.keycode);
            }
            keys.hold(now);
        })?;
        Ok(self.merge())
    }

    /// `slot`'s device `source` holds nothing: it left, or lost what it held.
    pub fn gone(&mut self, slot: Slot, source: u16) -> Delivered {
        self.slots[slot.0].gone(source);
        self.merge()
    }

    /// Every device `slot` carried holds nothing: its source ended.
    pub fn end(&mut self, slot: Slot) -> Delivered {
        self.slots[slot.0].end();
        self.merge()
    }

    fn merge(&mut self) -> Delivered {
        let held = self.slots.iter().flat_map(Devices::states);
        let union = held.fold(Usages::NONE, |all, keys| all.union(keys.held()));
        let before = self.union.held();
        Delivered { transitions: self.union.hold(union), held: before }
    }
}

/// The transitions one record made of the union, in [`Transitions`]' order.
pub struct Delivered {
    transitions: Transitions,
    /// The union as of the transition last yielded.
    held: Usages,
}

impl Iterator for Delivered {
    type Item = RawKeyEvent;

    fn next(&mut self) -> Option<RawKeyEvent> {
        let made = self.transitions.next()?;
        if made.pressed {
            self.held.insert(made.usage);
        } else {
            self.held.remove(made.usage);
        }
        let released = if made.pressed { 0 } else { MOD_RELEASED };
        Some(RawKeyEvent { keycode: made.usage, modifiers: modifiers(self.held) | released })
    }
}

fn modifiers(held: Usages) -> u8 {
    let either = |left: u8, right: u8| held.contains(left) || held.contains(right);
    (if either(0xE1, 0xE5) { MOD_SHIFT } else { 0 })
        | (if either(0xE0, 0xE4) { MOD_CTRL } else { 0 })
        | (if either(0xE2, 0xE6) { MOD_ALT } else { 0 })
        | (if either(0xE3, 0xE7) { MOD_GUI } else { 0 })
}

/// Counts per screen pixel's worth of the absolute space, before the
/// per-axis scale.
const REL_SCALE: i32 = 64;

/// The per-axis scale a screen of this size wants, chosen so a count moves
/// as far across as down: `x * width == y * height`. Per-axis, not a single
/// scalar: the absolute space is square and screens are not, so one shared
/// scalar would skew diagonal motion by the screen's aspect ratio.
fn rel_scale_for(width: u32, height: u32) -> (u16, u16) {
    let short = width.min(height) as i32;
    (
        (REL_SCALE * short / width.max(1) as i32).max(1) as u16,
        (REL_SCALE * short / height.max(1) as i32).max(1) as u16,
    )
}

/// The one pointer: each pointer's buttons ORed, and one position in the
/// absolute space, `0..=POINTER_ABSOLUTE_MAX` on each axis.
pub struct Pointer {
    /// Each pointer's buttons, by its slot and its record's `source`.
    buttons: [Devices<u8>; SLOTS],
    x: i32,
    y: i32,
    scale: (u16, u16),
}

/// What the one pointer is after a record.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct State {
    pub buttons: u8,
    pub scroll: i8,
    pub x: u16,
    pub y: u16,
}

impl Pointer {
    /// At the middle of a `width` by `height` screen, holding nothing.
    pub fn new(width: u32, height: u32) -> Self {
        let middle = (POINTER_ABSOLUTE_MAX as i32 + 1) / 2;
        Self { buttons: [const { Devices::EMPTY }; SLOTS], x: middle, y: middle, scale: rel_scale_for(width, height) }
    }

    /// The screen relative motion is scaled for.
    pub fn screen(&mut self, width: u32, height: u32) {
        self.scale = rel_scale_for(width, height);
    }

    /// `report` from `slot`: relative motion moves the position by the
    /// screen's scale, and absolute motion sets it.
    pub fn report(&mut self, slot: Slot, report: Report) -> Result<State, Full> {
        self.buttons[slot.0].change(report.source, |held| *held == 0, |held| *held = report.buttons)?;
        let most = i32::from(POINTER_ABSOLUTE_MAX);
        match report.motion {
            Motion::Relative { dx, dy } => {
                self.x = (self.x + i32::from(dx) * i32::from(self.scale.0)).clamp(0, most);
                self.y = (self.y + i32::from(dy) * i32::from(self.scale.1)).clamp(0, most);
            }
            Motion::Absolute { x, y } => (self.x, self.y) = (i32::from(x), i32::from(y)),
        }
        Ok(self.state(report.scroll))
    }

    /// `slot`'s pointer `source` holds nothing.
    pub fn gone(&mut self, slot: Slot, source: u16) -> State {
        self.buttons[slot.0].gone(source);
        self.state(0)
    }

    /// Every pointer `slot` carried holds nothing.
    pub fn end(&mut self, slot: Slot) -> State {
        self.buttons[slot.0].end();
        self.state(0)
    }

    fn state(&self, scroll: i8) -> State {
        let buttons = self.buttons.iter().flat_map(Devices::states).fold(0, |all, &held| all | held);
        // Both are clamped to `0..=POINTER_ABSOLUTE_MAX`.
        State { buttons, scroll, x: self.x as u16, y: self.y as u16 }
    }
}

#[cfg(test)]
mod tests {
    use super::rel_scale_for;

    /// Relative motion lands in a square space mapped by width and height, so
    /// the counts per pixel differ per axis or the pointer skews by the aspect
    /// ratio.
    #[test]
    fn a_count_moves_as_far_across_as_down() {
        for (w, h) in [(1920u32, 1200u32), (1024, 768), (3840, 2160), (1200, 1920), (800, 800)] {
            let (sx, sy) = rel_scale_for(w, h);
            assert!(sx >= 1 && sy >= 1, "{w}x{h} scaled a count to nothing");
            let across = sx as u64 * w as u64;
            let down = sy as u64 * h as u64;
            // Each scale is a truncated integer, so each product carries under
            // one screen dimension of error.
            assert!(across.abs_diff(down) < (w + h) as u64, "{w}x{h} moves {across} across and {down} down per count");
        }
    }
}
