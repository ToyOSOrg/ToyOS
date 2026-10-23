//! The display's mode: the bounds every mode the desktop takes is held to,
//! and what a device's report of its preferred size becomes.

use core::fmt;

use toyos_abi::pci::LEND_WINDOW;

use crate::layout::{Chrome, TEXT_CELL};

/// A display mode, in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
}

impl Mode {
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    /// One 32-bit scanout of this mode, as the region it is lent in: whole
    /// pages of what a device is lent in.
    pub const fn scanout_bytes(self) -> u64 {
        let page = toyos_abi::syscall::SHM_DEVICE_PAGE;
        (self.width as u64 * self.height as u64 * 4).div_ceil(page) * page
    }

    fn holds(self, other: Self) -> bool {
        self.width >= other.width && self.height >= other.height
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{}", self.width, self.height)
    }
}

/// A terminal's grid, which the least mode must hold.
const TERMINAL: (i32, i32) = (80, 24);

/// The least mode: an 80x24 terminal of [`TEXT_CELL`]s in a window frame,
/// above the taskbar, at scale 1.
pub const LEAST: Mode = {
    let c = Chrome::DEFAULT;
    let w = TERMINAL.0 * TEXT_CELL.0 + c.chrome_w();
    let h = TERMINAL.1 * TEXT_CELL.1 + c.chrome_h() + c.taskbar;
    Mode::new(w as u32, h as u32)
};

/// The largest mode: the largest Intel publishes for the T14's display
/// engine (Intel ARK, Tiger Lake GT2 `8086:9A49`: DisplayPort 7680x4320).
pub const LARGEST: Mode = Mode::new(7680, 4320);

// A mode change lends the new scanout before it gives the old one back, so the
// window holds two of the largest. It holds them only together: pcidev places
// a lend first-fit, so a small scanout left where it split the window can leave
// no run for a mode past half of it, and that flip is declined with the old
// mode kept and said (`issues/a-lent-scanout-is-one-contiguous-run.md`).
const _: () = assert!(2 * LARGEST.scanout_bytes() <= LEND_WINDOW);

/// The mode a display starts in, from the size its device prefers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Start {
    /// The preferred size, inside the bounds.
    Preferred(Mode),
    /// The preferred size, clamped axis by axis into them.
    Clamped { preferred: Mode, mode: Mode },
    /// No enabled output with a size: the least mode.
    Least,
}

impl Start {
    /// `preferred` is the device's: `None` for an output it reports disabled.
    pub fn of(preferred: Option<Mode>) -> Self {
        let Some(preferred) = preferred.filter(|m| m.width > 0 && m.height > 0) else {
            return Self::Least;
        };
        let mode = Mode::new(
            preferred.width.clamp(LEAST.width, LARGEST.width),
            preferred.height.clamp(LEAST.height, LARGEST.height),
        );
        if mode == preferred {
            Self::Preferred(mode)
        } else {
            Self::Clamped { preferred, mode }
        }
    }

    pub fn mode(self) -> Mode {
        match self {
            Self::Preferred(mode) | Self::Clamped { mode, .. } => mode,
            Self::Least => LEAST,
        }
    }
}

/// Why a mode asked for after start is not taken. The current mode stays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    BelowLeast(Mode),
    PastLargest(Mode),
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BelowLeast(mode) => write!(f, "{mode} is below the least mode, {LEAST}"),
            Self::PastLargest(mode) => write!(f, "{mode} is past the largest mode, {LARGEST}"),
        }
    }
}

/// A mode asked for after start, by the device or by a person: taken whole,
/// or refused.
pub fn follow(wanted: Mode) -> Result<Mode, Refused> {
    if !wanted.holds(LEAST) {
        Err(Refused::BelowLeast(wanted))
    } else if !LARGEST.holds(wanted) {
        Err(Refused::PastLargest(wanted))
    } else {
        Ok(wanted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_least_mode_is_a_terminal_over_the_taskbar() {
        assert_eq!(LEAST, Mode::new(642, 446));
    }

    #[test]
    fn a_preferred_size_inside_the_bounds_is_the_mode() {
        assert_eq!(Start::of(Some(Mode::new(1280, 800))), Start::Preferred(Mode::new(1280, 800)));
        assert_eq!(Start::of(Some(LEAST)).mode(), LEAST);
        assert_eq!(Start::of(Some(LARGEST)).mode(), LARGEST);
    }

    /// A 1x host's zoomed window that already reported 640x360 boots at
    /// 642x446, and one past the largest at the largest, axis by axis.
    #[test]
    fn a_preferred_size_outside_the_bounds_is_clamped_axis_by_axis() {
        let preferred = Mode::new(640, 360);
        assert_eq!(Start::of(Some(preferred)), Start::Clamped { preferred, mode: Mode::new(642, 446) });
        let preferred = Mode::new(8000, 1000);
        assert_eq!(Start::of(Some(preferred)), Start::Clamped { preferred, mode: Mode::new(7680, 1000) });
    }

    #[test]
    fn no_output_or_no_size_starts_at_the_least_mode() {
        assert_eq!(Start::of(None), Start::Least);
        assert_eq!(Start::of(Some(Mode::new(0, 768))), Start::Least);
        assert_eq!(Start::Least.mode(), LEAST);
    }

    #[test]
    fn a_later_mode_is_taken_whole_or_refused() {
        assert_eq!(follow(Mode::new(1600, 900)), Ok(Mode::new(1600, 900)));
        assert_eq!(follow(LEAST), Ok(LEAST));
        assert_eq!(follow(LARGEST), Ok(LARGEST));
        for small in [Mode::new(641, 900), Mode::new(1600, 445), Mode::new(0, 0)] {
            assert_eq!(follow(small), Err(Refused::BelowLeast(small)));
        }
        for big in [Mode::new(7681, 900), Mode::new(1600, 4321)] {
            assert_eq!(follow(big), Err(Refused::PastLargest(big)));
        }
    }
}
