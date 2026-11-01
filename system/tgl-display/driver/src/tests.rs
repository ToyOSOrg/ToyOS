//! The driver against `model.rs`. Expected watermarks are worked by hand
//! from Vol 12's algorithm text. Its worked example ("Example method, block,
//! and line calculations") is not used as an oracle: it omits the "+1" that
//! the algorithm adds to method 1 and to linear blocks per line.

extern crate std;

use std::collections::BTreeMap;

use crate::model::{Engine, Pcode};
use crate::watermark::{self, Latencies, Level, Pipe, Plane, Tiling};
use crate::{keep_memory_low_power, regs, Refusal};

const T14_PLANE: Plane = Plane { width: 1920, bytes_per_pixel: 4, tiling: Tiling::Linear, allocation: 682 };
const T14_PIPE: Pipe = Pipe { htotal: 2080, pixel_rate_khz: 138_778 };

fn lvl(blocks: u64, lines: u64) -> Option<Level> {
    Some(Level { blocks, lines })
}

#[test]
fn the_t14s_pcode_words_are_its_latencies_with_three_us_added() {
    let found = Latencies::from_pcode(0x3333_3300, 0x706b_4633);
    assert_eq!(found, Latencies { zero: 3, above: [54, 54, 54, 54, 73, 110, 115].map(Some) });
}

#[test]
fn a_zero_level_above_zero_ends_the_valid_levels() {
    let found = Latencies::from_pcode(0x0012_3305, 0x0000_0040);
    assert_eq!(found, Latencies { zero: 5, above: [Some(0x33), Some(0x12), None, None, None, None, None] });
}

#[test]
fn every_t14_level_fits_and_level_zero_carries_sagv() {
    let latencies = Latencies::from_pcode(0x3333_3300, 0x706b_4633);
    let found = watermark::watermarks(&latencies, 11, &T14_PLANE, &T14_PIPE).expect("level 0 fits");
    // 16 blocks a line; ceiling[latency × 138.778 / 2080] lines of them.
    // Level 0 at 3 + 11 µs: 1 line; 54 µs: 4; 73 µs: 5; 110 and 115 µs: 8.
    assert!(found.tolerates_sagv);
    assert_eq!(found.zero, Level { blocks: 17, lines: 1 });
    assert_eq!(found.above, [lvl(65, 4), lvl(65, 4), lvl(65, 4), lvl(65, 4), lvl(81, 5), lvl(129, 8), lvl(129, 8)]);
}

#[test]
fn a_level_past_the_allocation_disables_it_and_every_level_above() {
    let latencies = Latencies::from_pcode(0x3333_3300, 0x706b_4633);
    // 129 blocks need 142 with Vol 12's tenth; 81 need 90.
    let plane = Plane { allocation: 100, ..T14_PLANE };
    let found = watermark::watermarks(&latencies, 11, &plane, &T14_PIPE).expect("level 0 fits");
    assert_eq!(found.above[4], lvl(81, 5));
    assert_eq!(found.above[5..], [None, None]);
}

#[test]
fn y_tiling_takes_its_four_line_minimum() {
    // Blocks per line: ceiling[(4 × 7680 / 512) + 1] / 4 = 61 / 4; minimum 61.
    let plane = Plane { tiling: Tiling::Y, ..T14_PLANE };
    assert_eq!(watermark::level(3, &plane, &T14_PIPE), lvl(62, 4));
}

#[test]
fn the_t14s_line_time_is_its_firmwares() {
    assert_eq!(watermark::line_time(&T14_PIPE), 120);
}

#[test]
fn the_t14s_gop_gets_levels_one_to_seven_and_its_surface_rearmed_last() {
    let engine = Engine::t14();
    let done = keep_memory_low_power(&engine, &engine).expect("the T14's GOP is programmed");
    assert!(done.pixel_rate_khz.abs_diff(138_778) < 139, "pixel rate {} kHz", done.pixel_rate_khz);
    assert_eq!((done.scanout.width, done.scanout.height), (1920, 1080));
    assert!(done.refresh_interval_nanos().abs_diff(16_666_667) < 20_000, "{} ns a frame", done.refresh_interval_nanos());
    assert_eq!(done.before[0], 0x8000_4011);
    let wm = |level| regs::plane_wm(0, 1, level);
    let expected = [
        (wm(0), 0x8000_4011),
        (wm(1), 0x8001_0041),
        (wm(2), 0x8001_0041),
        (wm(3), 0x8001_0041),
        (wm(4), 0x8001_0041),
        (wm(5), 0x8001_4051),
        (wm(6), 0x8002_0081),
        (wm(7), 0x8002_0081),
        (regs::plane_surf(0, 1), 0),
    ];
    assert_eq!(engine.written(), expected);
}

fn refused(engine: &Engine, why: Refusal) {
    assert_eq!(keep_memory_low_power(engine, engine).map(|_| ()), Err(why));
    assert_eq!(engine.written(), [], "a refusal writes no display register");
}

#[test]
fn a_second_transcoder_on_is_refused_untouched() {
    let engine = Engine::t14();
    engine.set(regs::trans_conf(1), 0xc000_0000);
    refused(&engine, Refusal::NotOnePlane("another transcoder is on"));
}

#[test]
fn a_cursor_on_is_refused_untouched() {
    let engine = Engine::t14();
    engine.set(regs::CUR_CTL_A, 0x27);
    refused(&engine, Refusal::NotOnePlane("the cursor is on"));
}

#[test]
fn a_yuv_plane_is_refused_untouched() {
    let engine = Engine::t14();
    engine.set(regs::plane_ctl(0, 1), 0x8000_0000 | 0b00010 << 23);
    refused(&engine, Refusal::Format(0b00010));
}

#[test]
fn a_stopped_pipe_is_refused_untouched() {
    let engine = Engine::t14();
    let r: BTreeMap<_, _> = engine.regs.borrow().clone();
    let stopped = Engine::new(r, None, Pcode { latency_first: 0, latency_second: 0, sagv_block_us: 0, busy_reads: 0, error: 0 });
    refused(&stopped, Refusal::PipeStopped);
}

#[test]
fn a_line_time_not_the_modes_is_refused_untouched() {
    let engine = Engine::t14();
    engine.set(regs::WM_LINETIME_A, 100);
    refused(&engine, Refusal::LineTime { computed: 120, firmware: 100 });
}

#[test]
fn a_mailbox_error_is_refused_with_no_display_register_written() {
    let engine = Engine::t14();
    let r: BTreeMap<_, _> = engine.regs.borrow().clone();
    let failing = Engine::new(r, Some(16_666_667), Pcode { latency_first: 0, latency_second: 0, sagv_block_us: 0, busy_reads: 1, error: 0x03 });
    refused(&failing, Refusal::Mailbox { command: 0x06, interface: 0x03 });
}

#[test]
fn a_plane_that_cannot_tolerate_sagv_is_left_as_found() {
    let engine = Engine::t14();
    // 17 blocks at 3 µs fit 20 with the tenth; level 0 at 14 µs needs the
    // same 17, so shrink the block time's margin away with a longer block.
    engine.set(regs::plane_buf_cfg(0, 1), 0x0014_0000);
    let r: BTreeMap<_, _> = engine.regs.borrow().clone();
    let long_block = Engine::new(r, Some(16_666_667), Pcode { latency_first: 0x3333_3300, latency_second: 0x706b_4633, sagv_block_us: 40, busy_reads: 0, error: 0 });
    refused(&long_block, Refusal::SagvIntolerant);
}

#[test]
fn level_zero_takes_the_sagv_block_time_and_the_levels_above_do_not() {
    let latencies = Latencies::from_pcode(0x3333_3300, 0x706b_4633);
    // Level 0 at 3 + 30 µs: ceiling[33 × 138.778 / 2080] = 3 lines, 48 + 1 blocks.
    let found = watermark::watermarks(&latencies, 30, &T14_PLANE, &T14_PIPE).expect("level 0 fits");
    assert_eq!(found.zero, Level { blocks: 49, lines: 3 });
    assert_eq!(found.above[0], lvl(65, 4));
}

#[test]
fn an_allocation_short_of_a_line_takes_method_one_below_a_line_time() {
    // 10 blocks hold less than a line's 16, and 3 µs is under the 15 µs line
    // time, so method 1: 3 × 138.778 × 4 / 512 + 1 = 4.25, so 5 + 1 blocks, 1 line.
    let plane = Plane { allocation: 10, ..T14_PLANE };
    assert_eq!(watermark::level(3, &plane, &T14_PIPE), lvl(6, 1));
}

#[test]
fn a_mailbox_that_stays_busy_is_refused_with_no_display_register_written() {
    let engine = Engine::t14();
    let r: BTreeMap<_, _> = engine.regs.borrow().clone();
    let stuck = Engine::new(r, Some(16_666_667), Pcode { latency_first: 0, latency_second: 0, sagv_block_us: 0, busy_reads: u32::MAX, error: 0 });
    refused(&stuck, Refusal::Mailbox { command: 0x06, interface: regs::RUN_BUSY });
}

#[test]
fn a_plane_rotated_90_is_refused_untouched() {
    let engine = Engine::t14();
    engine.set(regs::plane_ctl(0, 1), 0x8400_0009);
    refused(&engine, Refusal::Layout(0x8400_0009));
}

#[test]
fn a_decompressing_plane_is_refused_untouched() {
    let engine = Engine::t14();
    engine.set(regs::plane_ctl(0, 1), 0x8400_8008);
    refused(&engine, Refusal::Layout(0x8400_8008));
}
