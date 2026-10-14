//! Shared tone playback: a deterministic sine, into a muted master.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use cpal::traits::{DeviceTrait, StreamTrait};

#[path = "master.rs"]
mod master;

const FREQ_HZ: f64 = 440.0;
const DURATION_SECS: f64 = 3.0;
/// -42 dBFS, so a mute that failed silently would still be almost inaudible.
const AMPLITUDE: f64 = 256.0;

/// Play a deterministic 440Hz sine on `device` at `config` with the master
/// muted, and block until it has drained. The rows judge soundserver's
/// stream and counters, which the mute leaves intact.
///
/// The sample value is derived from the absolute sample index, not from
/// per-callback state, so the generated signal is identical regardless of
/// how the callback invocations are sized or timed.
pub fn play(device: &cpal::Device, config: cpal::StreamConfig) {
    master::silence(&master::connect());
    let sample_rate = config.sample_rate as f64;
    let channels = config.channels as usize;
    let total_samples = (sample_rate * DURATION_SECS) as u64;

    let done = Arc::new(AtomicBool::new(false));
    let done2 = done.clone();
    let position = Arc::new(AtomicU64::new(0));
    let failed = Arc::new(AtomicBool::new(false));
    let failed2 = failed.clone();

    let stream = device
        .build_output_stream(
            config,
            move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                let mut n = position.load(Ordering::Relaxed);
                for frame in data.chunks_exact_mut(channels) {
                    let value = if n < total_samples {
                        let phase = 2.0 * std::f64::consts::PI * FREQ_HZ * n as f64 / sample_rate;
                        (AMPLITUDE * phase.sin()) as i16
                    } else {
                        0
                    };
                    frame.fill(value);
                    n += 1;
                }
                position.store(n, Ordering::Relaxed);
                if n >= total_samples {
                    done2.store(true, Ordering::Relaxed);
                }
            },
            move |err| {
                eprintln!("audio error: {err}");
                failed2.store(true, Ordering::Relaxed);
            },
            None,
        )
        .expect("failed to build audio stream");

    stream.play().expect("failed to play");

    while !done.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    // Let the tail of the tone drain through soundserver and the device.
    std::thread::sleep(std::time::Duration::from_millis(200));

    drop(stream);
    assert!(!failed.load(Ordering::Relaxed), "the tone's stream reported an error, printed above");
}
