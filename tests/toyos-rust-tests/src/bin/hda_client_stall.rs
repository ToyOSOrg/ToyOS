//! A client that stops producing mid-stream, on a machine whose audio device
//! is a cyclic DMA ring.
//!
//! The stall is the whole actuator. soundd's mix loop may leave a freed period
//! unfilled while a streaming client is still producing it, and on
//! virtio-sound that costs nothing: a period soundd has not submitted is a
//! period the device does not have. HDA's engine owns every period for as long
//! as it runs and replays the ones nobody refilled, so a period held across a
//! lap is completed a second time — which is what killed soundd on the T14.
//!
//! Nothing in the ordinary tone clients reaches that state: they keep their
//! ring full, so soundd never defers at all (`deferred=0` on every `hda_tone`
//! run measured). This one empties it on purpose, for longer than the ring
//! takes to come round.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use toyos::ipc::{FrameRx, RxStep};
use toyos::poller::{Poller, READABLE};
use toyos_inspect::{Value, MAX_SNAPSHOT_BYTES, MSG_INSPECT, MSG_SNAPSHOT, SOUND};

const FREQ_HZ: f64 = 440.0;
const AMPLITUDE: f64 = 16000.0;

/// Periods of tone between stalls, and how long each stall lasts.
///
/// The stall has to outlast one lap of the device ring — 8 periods, 23.2 ms —
/// or the engine never reaches a period soundd is still holding. 60 ms is two
/// and a half laps, and the run stages eight of them so the test does not rest
/// on catching one window.
const STALL: Duration = Duration::from_millis(60);
const STALLS: u64 = 8;
const CALLBACKS_BETWEEN_STALLS: u64 = 60;

/// How long each wait here has before it panics by name. A hang ceiling: what
/// each waits on is a quarter of a second or a lap of the ring away.
const WITHIN: Duration = Duration::from_secs(5);

fn main() {
    play(STALLS);
    // A second stream over the same device, after soundd has drained and
    // suspended: on a ring the drain gives the periods up rather than holding
    // them, so what the resume primes and where in the ring it starts are both
    // state the first stream left behind.
    await_suspended();
    play(2);
    println!("stalled {STALLS} then 2 times, soundd survived");
}

/// One stream of `stalls + 1` stretches of tone with a stall after each but
/// the last, closed when the last has played: the laps of the ring that follow
/// the last stall are played to a client that is still there, as every other
/// stall's are.
fn play(stalls: u64) {
    let host = cpal::default_host();
    let device = host.default_output_device().expect("no audio output device");
    let config = device.default_output_config().expect("no audio config");
    let sample_rate = config.sample_rate() as f64;
    let channels = config.channels() as usize;

    let (ended, stretch_ended) = mpsc::channel();
    let mut n: u64 = 0;
    let mut callbacks: u64 = 0;

    let stream = device
        .build_output_stream(
            config.into(),
            move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                for frame in data.chunks_exact_mut(channels) {
                    let phase = 2.0 * std::f64::consts::PI * FREQ_HZ * n as f64 / sample_rate;
                    frame.fill((AMPLITUDE * phase.sin()) as i16);
                    n += 1;
                }
                callbacks += 1;
                let stretch = callbacks / CALLBACKS_BETWEEN_STALLS;
                if callbacks % CALLBACKS_BETWEEN_STALLS != 0 || stretch > stalls + 1 {
                    return;
                }
                if stretch <= stalls {
                    std::thread::sleep(STALL);
                }
                ended.send(()).expect("`play` holds the receiver until the last stretch ends");
            },
            |err| eprintln!("audio error: {err}"),
            None,
        )
        .expect("failed to build audio stream");

    stream.play().expect("failed to play");
    let stretches = stalls + 1;
    for stretch in 1..=stretches {
        if let Err(why) = stretch_ended.recv_timeout(WITHIN) {
            panic!("stretch {stretch} of {stretches} did not end within {WITHIN:?}: {why}");
        }
    }
    drop(stream);
}

/// Wait until soundd says its device stream is stopped.
///
/// soundd tells no client that it suspended: its `inspect` answer is the one
/// place a client reads it. The mix loop publishes that once a wake, and the
/// device playing its tail out wakes it once a period, so a period is how
/// often it is asked.
fn await_suspended() {
    let deadline = Instant::now() + WITHIN;
    loop {
        let sound = inspect_sound();
        let (Some(Value::Text(state)), Some(&Value::U64(frames)), Some(&Value::U64(rate))) = (
            sound.get("sound.stream.state"),
            sound.get("sound.period_frames"),
            sound.get("sound.rate_hz"),
        ) else {
            panic!("soundd's snapshot names no stream state and period: {sound:?}");
        };
        if state == "suspended" {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "soundd's stream still reads `{state}` {WITHIN:?} after its only client closed, so \
             the second stream has no suspended daemon to resume"
        );
        std::thread::sleep(Duration::from_nanos(1_000_000_000 * frames / rate));
    }
}

/// soundd's own `inspect` answer, within [`WITHIN`].
fn inspect_sound() -> std::collections::BTreeMap<String, Value> {
    let conn = toyos::endow::service(SOUND.port).expect("a connection to soundd");
    conn.signal(MSG_INSPECT).expect("soundd takes an inspect request");
    let poller = Poller::new(1);
    let mut rx: Box<FrameRx<MAX_SNAPSHOT_BYTES>> = Box::new(FrameRx::new());
    let deadline = Instant::now() + WITHIN;
    loop {
        match rx.pump(&conn) {
            RxStep::Frame { msg_type: MSG_SNAPSHOT, payload_len } => {
                return toyos_inspect::decode(rx.payload(payload_len), SOUND)
                    .unwrap_or_else(|why| panic!("soundd's snapshot: {why}"));
            }
            RxStep::Idle => {}
            other => panic!("soundd answered inspect with {other:?}, not a snapshot"),
        }
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "soundd did not answer inspect within {WITHIN:?}");
        poller.watch(&conn, READABLE, 0);
        poller.wait(1, left.as_nanos() as u64, |_| {});
    }
}
