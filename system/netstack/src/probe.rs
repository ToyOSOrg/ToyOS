//! MEASUREMENT ONLY, never lands: what each pass found, for the `netstack: probe` line said every
//! [`EVERY`] while anything moved: the I219's messages, the frames each pass took, the passes no
//! message began that found frames, the longest gap between two passes, and the MAC's own counts.

use std::cell::Cell;
use std::time::{Duration, Instant};

pub const EVERY: Duration = Duration::from_millis(250);

#[derive(Default, Clone, Copy, PartialEq)]
pub struct Totals {
    pub passes: u64,
    pub msg_passes: u64,
    pub messages: u64,
    pub frames: u64,
    pub quiet_passes_with_frames: u64,
    pub quiet_frames: u64,
    pub b0: u64,
    pub b1_8: u64,
    pub b9_32: u64,
    pub b33_128: u64,
    pub b129: u64,
}

thread_local! {
    pub static TOTALS: Cell<Totals> = Cell::new(Totals::default());
    /// Messages the pass now running began with.
    pub static MESSAGES: Cell<u32> = const { Cell::new(0) };
    pub static MAX_FRAMES: Cell<u64> = const { Cell::new(0) };
    pub static MAX_GAP_US: Cell<u64> = const { Cell::new(0) };
    pub static MAX_GAP_FRAMES_US: Cell<u64> = const { Cell::new(0) };
    pub static LAST_PASS: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// A pass began with `messages` taken.
pub fn began(messages: u32) {
    MESSAGES.set(messages);
}

/// The pass's receive took `frames`.
pub fn took(frames: u64) {
    let now = Instant::now();
    let gap = LAST_PASS.replace(Some(now)).map_or(0, |at| now.duration_since(at).as_micros() as u64);
    MAX_GAP_US.set(MAX_GAP_US.get().max(gap));
    if frames > 0 {
        MAX_GAP_FRAMES_US.set(MAX_GAP_FRAMES_US.get().max(gap));
    }
    MAX_FRAMES.set(MAX_FRAMES.get().max(frames));
    let messages = u64::from(MESSAGES.replace(0));
    let mut t = TOTALS.get();
    t.passes += 1;
    t.messages += messages;
    t.frames += frames;
    if messages > 0 {
        t.msg_passes += 1;
    } else if frames > 0 {
        t.quiet_passes_with_frames += 1;
        t.quiet_frames += frames;
    }
    match frames {
        0 => t.b0 += 1,
        1..=8 => t.b1_8 += 1,
        9..=32 => t.b9_32 += 1,
        33..=128 => t.b33_128 += 1,
        _ => t.b129 += 1,
    }
    TOTALS.set(t);
}

/// The line's words for the pass totals, and the maxima since the last line, which start again.
pub fn words() -> String {
    let t = TOTALS.get();
    format!(
        "passes={} msg_passes={} messages={} frames={} quiet_passes_with_frames={} quiet_frames={} \
         b0={} b1_8={} b9_32={} b33_128={} b129={} max_frames={} max_gap_us={} max_gap_frames_us={}",
        t.passes,
        t.msg_passes,
        t.messages,
        t.frames,
        t.quiet_passes_with_frames,
        t.quiet_frames,
        t.b0,
        t.b1_8,
        t.b9_32,
        t.b33_128,
        t.b129,
        MAX_FRAMES.replace(0),
        MAX_GAP_US.replace(0),
        MAX_GAP_FRAMES_US.replace(0),
    )
}
