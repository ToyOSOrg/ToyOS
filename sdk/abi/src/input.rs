//! Wire-format types for HID input devices (keyboard, mouse).
//!
//! These cross the kernel→userland boundary as reads on a device handle, and
//! a source's connection to the owner of input (`toyos-input`).

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

/// [`MouseEvent::kind`]: `x` and `y` are a movement, in the device's counts.
pub const POINTER_RELATIVE: u8 = 0;
/// [`MouseEvent::kind`]: `x` and `y` are a position, each in
/// `0..=`[`POINTER_ABSOLUTE_MAX`].
pub const POINTER_ABSOLUTE: u8 = 1;
/// The far edge of an absolute axis.
pub const POINTER_ABSOLUTE_MAX: i16 = 32767;

crate::user_safe! {
    /// One pointer report, as the device gave it: nothing is integrated or
    /// merged before the owner of input, which ORs every pointer's buttons
    /// and keeps the one position (`toyos_input::Pointer`).
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct MouseEvent {
        /// Which of its sender's pointers this is.
        pub source: u8,
        /// This pointer's own buttons, in HID boot-mouse order: bit 0 left,
        /// 1 right, 2 middle, then two more.
        pub buttons: u8,
        /// Wheel notches, away from the user positive.
        pub scroll: i8,
        /// [`POINTER_RELATIVE`] or [`POINTER_ABSOLUTE`].
        pub kind: u8,
        pub x: i16,
        /// Down the screen positive, in either kind.
        pub y: i16,
    }
}
