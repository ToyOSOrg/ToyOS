//! The handoff squares: inside the scanout on every mode that gets them, apart
//! from each other, and absent on a mode too small for the row.

use toyos_bootmap::mark::{square, Scanout, Step, GAP, SIDE};

/// The T14's panel as its firmware hands it over.
const T14: Scanout = Scanout { width: 1920, height: 1080, stride: 1920, bytes: 0x7e9000 };

fn pixels(step: Step, scanout: Scanout) -> Vec<u64> {
    square(step, scanout).expect("this mode holds the row").collect()
}

#[test]
fn every_square_is_inside_the_scanout_and_whole() {
    for scanout in [T14, Scanout { width: 800, height: 600, stride: 832, bytes: 832 * 600 * 4 }] {
        for step in Step::ALL {
            let at = pixels(step, scanout);
            assert_eq!(at.len(), (SIDE * SIDE) as usize);
            assert!(at.iter().all(|&offset| offset + 4 <= scanout.bytes), "{step:?} on {scanout:?}");
            assert!(at.iter().all(|&offset| offset % 4 == 0));
            // Every pixel in the top rows, and none past the visible width.
            for offset in at {
                let (y, x) = (offset / 4 / u64::from(scanout.stride), offset / 4 % u64::from(scanout.stride));
                assert!((u64::from(GAP)..u64::from(GAP + SIDE)).contains(&y));
                assert!(x < u64::from(scanout.width - GAP));
            }
        }
    }
}

#[test]
fn the_squares_stand_apart_in_step_order_from_the_left() {
    let lefts: Vec<u64> = Step::ALL.iter().map(|&step| pixels(step, T14)[0] / 4).collect();
    assert_eq!(lefts, [16 * 1920 + 1776, 16 * 1920 + 1824, 16 * 1920 + 1872]);
}

#[test]
fn a_mode_that_cannot_hold_the_row_gets_no_square() {
    let narrow = Scanout { width: 159, height: 1080, stride: 159, bytes: 159 * 1080 * 4 };
    assert!(square(Step::KernelEntered, narrow).is_none());
    let short = Scanout { width: 1920, height: 63, stride: 1920, bytes: 1920 * 63 * 4 };
    assert!(square(Step::KernelEntered, short).is_none());
    let underfilled = Scanout { bytes: T14.bytes - 4, ..T14 };
    assert!(square(Step::BootServicesExited, underfilled).is_none());
    let stride_short = Scanout { stride: 1919, ..T14 };
    assert!(square(Step::BootMapLive, stride_short).is_none());
    let past_an_address = Scanout { width: u32::MAX, height: u32::MAX, stride: u32::MAX, bytes: u64::MAX };
    assert!(square(Step::BootMapLive, past_an_address).is_none());
}
