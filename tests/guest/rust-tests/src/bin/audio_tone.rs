//! Play a deterministic 440Hz sine to the end, at the device's own rate.

use cpal::traits::{DeviceTrait, HostTrait};

#[path = "../tone.rs"]
mod tone;

fn main() {
    let device = cpal::default_host().default_output_device().expect("no audio output device");
    let config = device.default_output_config().expect("no audio config");
    tone::play(&device, config.into());
    println!("tone done");
}
