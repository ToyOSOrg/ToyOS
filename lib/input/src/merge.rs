//! The owner of input's merge: every source's keys into one keyboard, and
//! every pointer's buttons and motion into one pointer.

use toyos_abi::input::{RawKeyEvent, MOD_ALT, MOD_CTRL, MOD_GUI, MOD_RELEASED, MOD_SHIFT, POINTER_ABSOLUTE_MAX};

use crate::keys::{Keys, Transitions, Usages};
use crate::wire::{Motion, Report, CONNECTIONS};

/// The sources the owner merges: the kernel's streams, until usbd takes the
/// last kernel source, and one per connection on the port.
pub const SLOTS: usize = CONNECTIONS + 1;

/// Which source a record came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Slot(usize);

impl Slot {
    /// The kernel's keyboard and mouse streams.
    pub const KERNEL: Self = Self(0);

    /// The `n`th connection on the port, of [`CONNECTIONS`].
    pub fn connection(n: usize) -> Option<Self> {
        (n < CONNECTIONS).then_some(Self(n + 1))
    }
}

/// One keyboard made of every source's: a key is held while any source holds
/// it.
#[derive(Default)]
pub struct Merge {
    sources: [Keys; SLOTS],
    union: Keys,
}

impl Merge {
    /// `key` from `slot`: what it changed of the union, each transition with
    /// the union's modifiers after it. A repeated make, or a key another
    /// source already holds, changes nothing.
    pub fn key(&mut self, slot: Slot, key: RawKeyEvent) -> Delivered {
        let mut now = self.sources[slot.0].held();
        if key.pressed() {
            now.insert(key.keycode);
        } else {
            now.remove(key.keycode);
        }
        self.sources[slot.0].hold(now);
        self.merge()
    }

    /// `slot` holds nothing: its source ended, or lost what it held.
    pub fn end(&mut self, slot: Slot) -> Delivered {
        self.sources[slot.0].hold(Usages::NONE);
        self.merge()
    }

    fn merge(&mut self) -> Delivered {
        let union = self.sources.iter().fold(Usages::NONE, |all, keys| all.union(keys.held()));
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
pub fn rel_scale_for(width: u32, height: u32) -> (u16, u16) {
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
    buttons: [[u8; 256]; SLOTS],
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
        Self { buttons: [[0; 256]; SLOTS], x: middle, y: middle, scale: rel_scale_for(width, height) }
    }

    /// The screen relative motion is scaled for.
    pub fn screen(&mut self, width: u32, height: u32) {
        self.scale = rel_scale_for(width, height);
    }

    /// `report` from `slot`: relative motion moves the position by the
    /// screen's scale, and absolute motion sets it.
    pub fn report(&mut self, slot: Slot, report: Report) -> State {
        self.buttons[slot.0][usize::from(report.source)] = report.buttons;
        let most = i32::from(POINTER_ABSOLUTE_MAX);
        match report.motion {
            Motion::Relative { dx, dy } => {
                self.x = (self.x + i32::from(dx) * i32::from(self.scale.0)).clamp(0, most);
                self.y = (self.y + i32::from(dy) * i32::from(self.scale.1)).clamp(0, most);
            }
            Motion::Absolute { x, y } => (self.x, self.y) = (i32::from(x), i32::from(y)),
        }
        self.state(report.scroll)
    }

    /// Every pointer `slot` carried holds nothing.
    pub fn end(&mut self, slot: Slot) -> State {
        self.buttons[slot.0] = [0; 256];
        self.state(0)
    }

    fn state(&self, scroll: i8) -> State {
        let buttons = self.buttons.iter().flatten().fold(0, |all, &held| all | held);
        // Both are clamped to `0..=POINTER_ABSOLUTE_MAX`.
        State { buttons, scroll, x: self.x as u16, y: self.y as u16 }
    }
}
