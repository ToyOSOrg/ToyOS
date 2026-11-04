//! Wait until `/system/bin/tgl-display` has allowed DC6, with PG3 and the
//! display codec's audio well down, then play `audio_tone`'s tone through
//! `hda` to its end.
//!
//! The `display_output_presents` metal row runs it on the T14: the tone is
//! its evidence that `hda`'s analog codec plays on with PG3 down.

use cpal::traits::{DeviceTrait, HostTrait};
use toyos_logstream::program_line;

#[path = "../served_log.rs"]
mod served_log;
#[path = "../tone.rs"]
mod tone;

fn main() {
    served_log::Log::open().until("tgl-display's DC6 allow", served_log::START_LINE, |line| {
        program_line(line).is_some_and(|said| {
            assert!(!said.text.contains("DC5 and DC6 stay disallowed"), "tgl-display refused DC6: {}", said.text);
            said.tag == "tgl-display" && said.text.contains("DMC package")
        })
    });
    let device = cpal::default_host().default_output_device().expect("no audio output device");
    let config = device.default_output_config().expect("no audio config");
    tone::play(&device, config.into());
    println!("tone done");
}
