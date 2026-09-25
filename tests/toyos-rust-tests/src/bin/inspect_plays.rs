//! Plays periods through soundd and says how many frames soundd provably took,
//! for `inspect_reads_its_owners` to hold `sound.periods.*` against.
//!
//! **What is proven taken is what the ring says, not what was written.** A
//! slot can be filled only once soundd has emptied it, so of `fills` slots
//! filled into a ring of `slots`, at least `fills - slots` were taken, each in
//! a mix pass that submitted the periods it went out in and then published its
//! counters. soundd signals every client at the top of every pass and a read
//! drains every signal waiting, so the second signal read after the last
//! counted fill was written by a pass that began after that fill: every pass
//! that took a counted slot had published by then.
//!
//! At the device's own rate and channel count, so a client frame is a device
//! frame and no resampler stands between the two counts.

use toyos::audio::{AudioStream, FORMAT_S16LE};

const RATE: u32 = 44_100;
const CHANNELS: u16 = 2;
/// Slots filled past the ring's first fill: every one is a slot soundd took.
const TAKEN_SLOTS: u64 = 32;

fn main() {
    let mut stream = AudioStream::open(RATE, CHANNELS, FORMAT_S16LE).expect("open a stream on soundd");
    assert_eq!(stream.device_sample_rate(), RATE, "the device is not at the client's rate");
    assert_eq!(stream.device_channels(), CHANNELS, "the device is not the client's channel count");
    let period_frames = u64::from(stream.period_frames());

    // The first signal comes with the stream and finds the ring empty: that
    // fill is the ring's own size, and says nothing about what soundd took.
    let mut slots = 0u64;
    stream
        .wait_and_fill(|buf| {
            buf.fill(0x11);
            slots += 1;
        })
        .expect("the first fill");
    let mut fills = slots;
    while fills < slots + TAKEN_SLOTS {
        stream
            .wait_and_fill(|buf| {
                buf.fill(0x11);
                fills += 1;
            })
            .expect("soundd went away mid-stream");
    }
    for _ in 0..2 {
        stream.wait_and_fill(|buf| buf.fill(0)).expect("soundd went away before publishing");
    }
    stream.close();
    println!("inspect plays: soundd took {} frames", (fills - slots) * period_frames);
}
