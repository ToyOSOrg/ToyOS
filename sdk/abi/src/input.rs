//! A key transition and a pointer's axes, as the surface protocol and the
//! owner of input (`toyos-input`) carry them.

/// Keyboard modifier flags.
pub const MOD_SHIFT: u8 = 1;
pub const MOD_CTRL: u8 = 2;
pub const MOD_ALT: u8 = 4;
pub const MOD_GUI: u8 = 8;
pub const MOD_RELEASED: u8 = 0x10;

crate::user_safe! {
    /// One key transition: which physical key moved, by its HID Keyboard-page
    /// usage, and which way. There is no layout in it, so there is nothing
    /// here that a layout could change.
    ///
    /// **From a source, `modifiers` is `MOD_RELEASED` or nothing**: a source
    /// knows one keyboard's keys. The owner of input merges every source's
    /// (`toyos_input::Merge`) and hands on each transition with the modifier
    /// mask across every keyboard, so Shift held on one keyboard and a letter
    /// typed on another is a capital.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct RawKeyEvent {
        pub keycode: u8,
        pub modifiers: u8,
    }
}

impl RawKeyEvent {
    pub fn pressed(&self) -> bool { self.modifiers & MOD_RELEASED == 0 }
    pub fn released(&self) -> bool { self.modifiers & MOD_RELEASED != 0 }
    pub fn shift(&self) -> bool { self.modifiers & MOD_SHIFT != 0 }
    pub fn ctrl(&self) -> bool { self.modifiers & MOD_CTRL != 0 }
    pub fn alt(&self) -> bool { self.modifiers & MOD_ALT != 0 }
    pub fn gui(&self) -> bool { self.modifiers & MOD_GUI != 0 }
}

/// A pointer report's kind: `x` and `y` are a movement, in the device's counts.
pub const POINTER_RELATIVE: u8 = 0;
/// A pointer report's kind: `x` and `y` are a position, each in
/// `0..=`[`POINTER_ABSOLUTE_MAX`].
pub const POINTER_ABSOLUTE: u8 = 1;
/// The far edge of an absolute axis.
pub const POINTER_ABSOLUTE_MAX: i16 = 32767;
