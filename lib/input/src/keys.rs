//! One keyboard's held keys, and the transitions between two sets of them.
//!
//! A transition is the difference between two sets of **one** keyboard's
//! keys: diffing against another's would release keys still down. A usage
//! held twice is held once.

/// The first modifier's usage: LeftControl, then LeftShift, LeftAlt,
/// LeftGUI and their right-hand four (HID Usage Tables, Keyboard/Keypad page).
pub const MODIFIER: u8 = 0xE0;

/// A set of Keyboard/Keypad page usages, one bit each.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Usages([u64; 4]);

fn bit(usage: u8) -> (usize, u64) {
    (usize::from(usage / 64), 1 << (usage % 64))
}

/// The eight modifiers' bits, which sit in the last word.
const MODIFIERS: Usages = Usages([0, 0, 0, 0xFF << (MODIFIER % 64)]);

impl Usages {
    pub const NONE: Self = Self([0; 4]);

    pub fn contains(self, usage: u8) -> bool {
        let (word, mask) = bit(usage);
        self.0[word] & mask != 0
    }

    pub fn insert(&mut self, usage: u8) {
        let (word, mask) = bit(usage);
        self.0[word] |= mask;
    }

    pub fn remove(&mut self, usage: u8) {
        let (word, mask) = bit(usage);
        self.0[word] &= !mask;
    }

    pub fn union(self, other: Self) -> Self {
        Self(core::array::from_fn(|w| self.0[w] | other.0[w]))
    }

    /// The usages in exactly one of `a` and `b`.
    pub fn symmetric(a: Self, b: Self) -> Self {
        Self(core::array::from_fn(|w| a.0[w] ^ b.0[w]))
    }

    /// How many usages are held.
    pub fn count(self) -> u32 {
        self.0.iter().map(|w| w.count_ones()).sum()
    }

    /// These, but none of `other`.
    pub fn without(self, other: Self) -> Self {
        Self(core::array::from_fn(|w| self.0[w] & !other.0[w]))
    }
}

/// One key went down or up, named by its Keyboard/Keypad page usage.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Transition {
    pub usage: u8,
    pub pressed: bool,
}

/// One keyboard's keys held, and by `Default` none.
///
/// Not `Copy`: a copy would hold keys the keyboard itself goes on to release.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Keys {
    held: Usages,
}

impl Keys {
    /// What is held.
    pub fn held(&self) -> Usages {
        self.held
    }

    /// Hold `now` and nothing else, and answer the transitions that made it.
    pub fn hold(&mut self, now: Usages) -> Transitions {
        let changed = Usages(core::array::from_fn(|w| self.held.0[w] ^ now.0[w]));
        self.held = now;
        Transitions { changed, now, pass: Pass::Modifiers }
    }
}

/// Which transitions [`Transitions`] is yielding. **Modifiers first**, so a
/// set pressing Shift and a key together is a shifted key and one pressing
/// Ctrl, Alt and D together is the chord; then releases, then presses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pass {
    Modifiers,
    Released,
    Pressed,
}

/// The transitions one change of set made, each usage at most once and in
/// ascending usage within each pass.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Transitions {
    changed: Usages,
    now: Usages,
    pass: Pass,
}

impl Iterator for Transitions {
    type Item = Transition;

    fn next(&mut self) -> Option<Transition> {
        loop {
            // The modifiers' pass has cleared their bits before the others run.
            let wanted = |w: usize| match self.pass {
                Pass::Modifiers => self.changed.0[w] & MODIFIERS.0[w],
                Pass::Released => self.changed.0[w] & !self.now.0[w],
                Pass::Pressed => self.changed.0[w] & self.now.0[w],
            };
            if let Some(w) = (0..4).find(|&w| wanted(w) != 0) {
                let at = wanted(w).trailing_zeros();
                self.changed.0[w] &= !(1 << at);
                // `w < 4` and `at < 64`, so the usage is a byte.
                let usage = (w as u32 * 64 + at) as u8;
                return Some(Transition { usage, pressed: self.now.0[w] & (1 << at) != 0 });
            }
            self.pass = match self.pass {
                Pass::Modifiers => Pass::Released,
                Pass::Released => Pass::Pressed,
                Pass::Pressed => return None,
            };
        }
    }
}
