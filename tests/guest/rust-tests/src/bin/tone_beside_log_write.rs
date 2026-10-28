//! `hda_tone`'s tone, played while a thread writes lines to `/log` on the
//! boot's log disk ([`log_lines`]): `tone_beside_a_log_write`'s stimulus.
//! The judge counts the lines read back off the log, which have to be a
//! mebibyte; soundserver's counters over the job's window are the verdict.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait};

#[path = "../tone.rs"]
mod tone;

#[path = "../log_lines.rs"]
mod log_lines;

fn main() {
    let device = cpal::default_host().default_output_device().expect("no audio output device");
    let config = device.default_output_config().expect("no audio config");
    let playing = Arc::new(AtomicBool::new(true));
    let writer = {
        let playing = playing.clone();
        std::thread::spawn(move || log_lines::say("tone_beside_log_write", u64::MAX, 2000, || playing.load(Ordering::Relaxed)))
    };
    tone::play(&device, config.into());
    playing.store(false, Ordering::Relaxed);
    let said = writer.join().expect("tone_beside_log_write: the writer panicked");
    println!("tone done");
    println!("tone_beside_log_write: {said} lines of {} bytes said beside the tone", log_lines::LINE_BYTES);
}
