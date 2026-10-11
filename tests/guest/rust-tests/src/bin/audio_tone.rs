//! Play a deterministic 440Hz sine to the end.

#[path = "../tone.rs"]
mod tone;

fn main() {
    tone::play_tone();
    println!("tone done");
}
