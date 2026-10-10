//! One stream through soundserver's own virtio-sound driver, judged by what
//! soundserver counted and in what state it left the device — never by how long
//! anything took.
//!
//! soundserver suspends only once every period it submitted has come back from
//! the device, so a stream that ends with soundserver suspended is one whose
//! every submitted period completed; and the periods it submitted cover at
//! least the periods this client filled. With no client its wait has no
//! timeout, so the suspend is reached only through the claim's interrupts: a
//! transmit queue with no vector leaves it `running`.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use toyos_inspect::{Value, SOUND};

/// Periods of tone the client fills: a few laps of the eight-period pipeline.
const PERIODS: u64 = 64;

/// A hang ceiling on each wait here, none of which is more than a second of
/// audio away.
const WITHIN: Duration = Duration::from_secs(30);

const FREQ_HZ: f64 = 440.0;
const AMPLITUDE: f64 = 16000.0;

/// What soundserver's `inspect` answer says of its device and stream.
struct Sound {
    device: String,
    state: String,
    clients: u64,
    submitted: u64,
    period_frames: u64,
    rate: u64,
}

fn sound() -> Sound {
    let answer = inspect::ask(SOUND).unwrap_or_else(|why| panic!("soundserver's inspect answer: {why}"));
    let text = |path: &str| match answer.get(path) {
        Some(Value::Text(text)) => text.clone(),
        other => panic!("soundserver's snapshot has no text at {path}: {other:?}"),
    };
    let number = |path: &str| match answer.get(path) {
        Some(&Value::U64(n)) => n,
        other => panic!("soundserver's snapshot has no number at {path}: {other:?}"),
    };
    Sound {
        device: text("sound.device"),
        state: text("sound.stream.state"),
        clients: number("sound.stream.clients"),
        submitted: number("sound.periods.submitted"),
        period_frames: number("sound.period_frames"),
        rate: number("sound.rate_hz"),
    }
}

fn main() {
    let before = sound();
    assert_eq!(before.device, "virtio-sound", "soundserver drives no virtio-sound device");
    assert_eq!(
        (before.state.as_str(), before.clients),
        ("suspended", 0),
        "soundserver had a stream before this client's: no count here is this client's alone"
    );

    let filled = play(PERIODS * before.period_frames);

    // soundserver says nothing to a client that left; its answer is where the
    // suspend is read, and the device playing its tail out wakes it once a
    // period.
    let deadline = Instant::now() + WITHIN;
    let after = loop {
        let now = sound();
        if now.state == "suspended" && now.clients == 0 {
            break now;
        }
        assert!(
            Instant::now() < deadline,
            "soundserver's stream still reads `{}` with {} client(s) {WITHIN:?} after its only client \
             closed: a submitted period never came back",
            now.state,
            now.clients
        );
        std::thread::sleep(Duration::from_nanos(1_000_000_000 * now.period_frames / now.rate));
    };

    let submitted = after.submitted - before.submitted;
    let covered = filled / before.period_frames;
    assert!(
        submitted >= covered,
        "soundserver submitted {submitted} period(s) for a client that filled {covered}"
    );
    println!(
        "virtio_sound_counts: {submitted} period(s) submitted for {covered} filled, every one \
         completed before the stream stopped"
    );
}

/// Play a tone until `frames` have been filled, and close the stream: the
/// frames filled.
fn play(frames: u64) -> u64 {
    let host = cpal::default_host();
    let device = host.default_output_device().expect("no audio output device");
    let config = device.default_output_config().expect("no audio config");
    let sample_rate = config.sample_rate() as f64;
    let channels = config.channels() as usize;
    let (done, filled) = mpsc::channel();
    let mut n: u64 = 0;
    let stream = device
        .build_output_stream(
            config.into(),
            move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                for frame in data.chunks_exact_mut(channels) {
                    let phase = 2.0 * std::f64::consts::PI * FREQ_HZ * n as f64 / sample_rate;
                    frame.fill((AMPLITUDE * phase.sin()) as i16);
                    n += 1;
                }
                if n >= frames {
                    // The receiver is gone once one has been taken.
                    let _ = done.send(n);
                }
            },
            |err| panic!("the stream reported an error: {err}"),
            None,
        )
        .expect("failed to build audio stream");
    stream.play().expect("failed to play");
    let filled = filled
        .recv_timeout(WITHIN)
        .unwrap_or_else(|why| panic!("{frames} frames were not filled within {WITHIN:?}: {why}"));
    drop(stream);
    filled
}
