//! `hda_tone`'s tone, played while a thread writes the boot stick through
//! usbd: `tone_beside_a_stick_write`'s stimulus
//! (`issues/a-megabyte-written-to-the-stick-starves-a-tone-beside-it.md`).
//! It ends with how much it wrote, which has to be at least the mebibyte the
//! issue names; soundserver's counters over its window are the verdict.

use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait};

#[path = "../tone.rs"]
mod tone;

const PATH: &str = "/home/tone-beside.bin";
const SPAN: u64 = 8 << 20;
const CHUNK: usize = 256 << 10;
const SYNC_EVERY: u64 = 1 << 20;

fn main() {
    let device = cpal::default_host().default_output_device().expect("no audio output device");
    let config = device.default_output_config().expect("no audio config");
    let playing = Arc::new(AtomicBool::new(true));
    let writer = {
        let playing = playing.clone();
        std::thread::spawn(move || {
            let mut file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(PATH)
                .unwrap_or_else(|e| panic!("tone_beside_stick_write: {PATH}: {e}"));
            let chunk = vec![0x5au8; CHUNK];
            let mut written: u64 = 0;
            while playing.load(Ordering::Relaxed) {
                if written % SPAN == 0 {
                    file.seek(SeekFrom::Start(0)).expect("tone_beside_stick_write: seek");
                }
                file.write_all(&chunk).expect("tone_beside_stick_write: write");
                written += CHUNK as u64;
                if written % SYNC_EVERY == 0 {
                    file.sync_all().expect("tone_beside_stick_write: fsync");
                }
            }
            file.sync_all().expect("tone_beside_stick_write: the last fsync");
            written
        })
    };
    tone::play(&device, config.into());
    playing.store(false, Ordering::Relaxed);
    let written = writer.join().expect("tone_beside_stick_write: the writer panicked");
    println!("tone done");
    println!("tone_beside_stick_write: {} KiB written to the stick while the tone played", written >> 10);
    assert!(written >= 1 << 20, "tone_beside_stick_write: under a mebibyte written beside the tone");
}
