//! Play the same 440Hz sine to the end at a rate the device does not run:
//! the other of soundserver's two device rates, 44.1 kHz and 48 kHz, so
//! soundserver resamples it.

use cpal::traits::{DeviceTrait, HostTrait};

#[path = "../tone.rs"]
mod tone;

fn main() {
    let device = cpal::default_host().default_output_device().expect("no audio output device");
    let mut config: cpal::StreamConfig = device.default_output_config().expect("no audio config").into();
    config.sample_rate = match config.sample_rate {
        44_100 => 48_000,
        48_000 => 44_100,
        other => panic!("the device runs at {other} Hz, which is neither of soundserver's device rates"),
    };
    config.buffer_size = cpal::BufferSize::Default;
    println!("tone at {} Hz", config.sample_rate);
    tone::play(&device, config);
    println!("tone done");
}
