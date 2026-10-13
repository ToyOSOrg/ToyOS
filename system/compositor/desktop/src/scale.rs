//! How many panel pixels the desktop's pixel is, chosen from the panel's
//! density.
//!
//! ToyOS's own rule. The desktop's pixel is CSS Values 3 §5.2's reference
//! pixel, the visual angle of 1/96 inch at 28 inches, and every panel is
//! assumed viewed from 24 inches, so it is held to 96 x 28 / 24 = 112 dots
//! per inch. One distance for every panel makes the scale a function of
//! density alone; 24 inches is ToyOS's choice, not a published figure.

use core::fmt;

use crate::layout::Chrome;

/// A scale as 120ths, the representation the wire carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Scale(u16);

/// Every scale is a quarter step: 4/4 to 16/4.
const QUARTERS: core::ops::RangeInclusive<u32> = 4..=16;
const PER_QUARTER: u16 = 30;

/// The reference pixel's density at the assumed distance, as the ratio
/// `96 * 28 / 24`.
const TARGET_DPI: (u64, u64) = (96 * 28, 24);
/// Millimetres per inch, as a ratio.
const MM_PER_INCH: (u64, u64) = (254, 10);

/// A terminal's grid and its cell, which the least screen must hold.
const TERMINAL: (i32, i32) = (80, 24);
const CELL: (i32, i32) = (8, 16);

/// The least logical screen: an 80x24 terminal of 8x16 cells in a window
/// frame, above the taskbar.
pub const LEAST: (u32, u32) = {
    let c = Chrome::DEFAULT;
    let w = TERMINAL.0 * CELL.0 + c.chrome_w();
    let h = TERMINAL.1 * CELL.1 + c.chrome_h() + c.taskbar;
    (w as u32, h as u32)
};

impl Scale {
    pub const ONE: Self = Self(120);

    /// The 120ths, as the wire carries them.
    pub const fn n(self) -> u16 {
        self.0
    }

    fn quarters(q: u32) -> Self {
        Self(q as u16 * PER_QUARTER)
    }

    /// The scale for a `mode` in pixels on a panel of `mm`: the quarter step
    /// from 1 to 4 nearest its density over [`TARGET_DPI`], a tie going to the
    /// smaller, then the largest step at or below it that leaves the logical
    /// screen at least [`LEAST`]. A mode below [`LEAST`] at 1 gets 1.
    pub fn auto(mode: (u32, u32), mm: (u32, u32)) -> Self {
        // density / target = (px_diag / mm_diag) * (254/10) / (2688/24), compared
        // squared so no root is taken: ideal^2 = A / B.
        let px2 = u64::from(mode.0).pow(2) + u64::from(mode.1).pow(2);
        let mm2 = u64::from(mm.0).pow(2) + u64::from(mm.1).pow(2);
        let a = u128::from(px2) * u128::from(MM_PER_INCH.0 * TARGET_DPI.1).pow(2);
        let b = u128::from(mm2) * u128::from(MM_PER_INCH.1 * TARGET_DPI.0).pow(2);
        // Quarter q is nearest while ideal <= (2q + 1) / 8, i.e. 64 A <= (2q+1)^2 B.
        let nearest = QUARTERS
            .clone()
            .find(|&q| 64 * a <= u128::from(2 * q + 1).pow(2) * b)
            .unwrap_or(*QUARTERS.end());
        (*QUARTERS.start()..=nearest)
            .rev()
            .map(Self::quarters)
            .find(|s| s.holds_least(mode))
            .unwrap_or(Self::ONE)
    }

    /// Whether `mode` at this scale is at least [`LEAST`] in logical pixels.
    pub fn holds_least(self, mode: (u32, u32)) -> bool {
        let n = u64::from(self.0);
        u64::from(mode.0) * 120 >= u64::from(LEAST.0) * n
            && u64::from(mode.1) * 120 >= u64::from(LEAST.1) * n
    }
}

/// As a decimal: `1`, `1.25`, `1.5`, `1.75`.
impl fmt::Display for Scale {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (whole, frac) = (self.0 / 120, self.0 % 120);
        match frac {
            0 => write!(f, "{whole}"),
            30 => write!(f, "{whole}.25"),
            60 => write!(f, "{whole}.5"),
            90 => write!(f, "{whole}.75"),
            _ => unreachable!("a scale is a quarter step"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    fn auto(w: u32, h: u32, wmm: u32, hmm: u32) -> alloc::string::String {
        Scale::auto((w, h), (wmm, hmm)).to_string()
    }

    #[test]
    fn the_least_screen_is_a_terminal_over_the_taskbar() {
        assert_eq!(LEAST, (642, 446));
    }

    /// The panels M1 read: 309x173 mm and 309x174 mm at 1920x1080.
    #[test]
    fn the_benches_panels_are_one_and_a_half() {
        assert_eq!(auto(1920, 1080, 309, 173), "1.5");
        assert_eq!(auto(1920, 1080, 309, 174), "1.5");
    }

    /// PR #895's `scale-rule-r3.log` rows.
    #[test]
    fn each_row_of_the_rule_gives_its_step() {
        let rows = [
            ((1280, 800, 325, 203), "1"),
            ((1280, 720, 325, 183), "1"),
            ((1024, 768, 260, 195), "1"),
            ((1920, 1080, 310, 174), "1.5"),
            ((2560, 1600, 256, 160), "2.25"),
            ((3456, 2234, 340, 220), "2.25"),
            ((1280, 720, 128, 72), "1.5"),
            ((2560, 1440, 531, 299), "1"),
            ((3840, 2160, 598, 336), "1.5"),
            ((5120, 2880, 598, 336), "2"),
            ((3840, 2160, 509, 286), "1.75"),
            ((1920, 1080, 432, 243), "1"),
            ((1920, 1080, 443, 249), "1"),
        ];
        for ((w, h, wmm, hmm), want) in rows {
            assert_eq!(auto(w, h, wmm, hmm), want, "{w}x{h} on {wmm}x{hmm} mm");
        }
    }

    /// 2.25 is 254 dpi's nearest step and leaves 569x320 at 1280x720, so the
    /// step falls until 642x446 fits.
    #[test]
    fn a_small_mode_steps_down_to_the_least_screen() {
        assert_eq!(auto(1280, 720, 128, 72), "1.5");
        assert!(!Scale::quarters(7).holds_least((1280, 720)));
        assert!(Scale::quarters(6).holds_least((1280, 720)));
    }

    #[test]
    fn a_mode_below_the_least_screen_is_one() {
        assert_eq!(auto(640, 360, 163, 91), "1");
    }

    /// 1890x2520 on 381x508 mm is 126 dpi, 1.125 times the target: exactly
    /// between 1 and 1.25.
    #[test]
    fn a_tie_goes_to_the_smaller_step() {
        assert_eq!(auto(1890, 2520, 381, 508), "1");
        assert_eq!(auto(1890, 2520, 381, 507), "1.25");
    }

    #[test]
    fn the_largest_step_is_four() {
        assert_eq!(auto(7680, 4320, 100, 56), "4");
    }

    #[test]
    fn a_scale_reads_as_a_decimal() {
        let shown: alloc::vec::Vec<_> = QUARTERS.map(|q| Scale::quarters(q).to_string()).collect();
        assert_eq!(shown[..5], ["1", "1.25", "1.5", "1.75", "2"]);
        assert_eq!(shown[12], "4");
    }
}
