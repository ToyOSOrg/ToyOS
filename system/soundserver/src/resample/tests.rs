//! The resampler against the analytic sine, from every client rate in common
//! use to both device rates, on every interpolator rubato can take on a CPU
//! ToyOS runs on, each built explicitly; and what [`build`] makes, which is
//! what a stream runs, is one of them.
//!
//! Each tone's output is fitted by IEEE 1057's three-parameter fit at its
//! known frequency: the fitted amplitude is the gain, and what the fit leaves
//! is THD+N, unweighted over the whole output band.

use std::f64::consts::PI;

use rubato::{Resampler, SincFixedOut};

use super::interpolators::interpolators;
use super::{build, parameters, INTERPOLATION, MAX_RATIO_RELATIVE};

/// The device period, which is the resampler's fixed output chunk.
const PERIOD: usize = 128;
const CHANNELS: usize = 2;
/// The test tone, as AES17 measures THD+N.
const TONE_DBFS: f64 = -1.0;
/// Every client rate in common use, from the lowest soundserver accepts to the highest.
const CLIENTS: [u32; 12] =
    [8000, 11025, 16000, 22050, 24000, 32000, 44100, 48000, 88200, 96000, 176400, 192000];
/// The TI SRC4192/SRC4193's band edges, as fractions of the lower of the two rates.
const PASS_EDGE: f64 = 0.4535;
const STOP_EDGE: f64 = 0.5465;
/// The SRC4192's interpolation-filter ripple.
const RIPPLE_DB: f64 = 0.007;
/// SoX's `rate` at "high" quality: 16-bit mastering with dither.
const REJECT_DB: f64 = 125.0;
/// Fitted output per tone.
const SECONDS: f64 = 0.5;

fn pairs() -> Vec<(u32, u32)> {
    let mut v = Vec::new();
    for device in toyos_mixer::DEVICE_RATES {
        for client in CLIENTS {
            if client != device {
                v.push((client, device));
            }
        }
    }
    v
}

fn amplitude() -> f64 {
    10f64.powf(TONE_DBFS / 20.0)
}

/// Every interpolator this CPU's architecture can take, each in a resampler
/// built as [`build`] builds it, with the sinc length and cutoff rubato's own
/// dispatch derives: the length rounded up to a multiple of 8, and the cutoff
/// scaled by the ratio below 1.
fn explicit(client: u32, device: u32) -> Vec<(&'static str, SincFixedOut<f32>)> {
    let p = parameters(client, device);
    let ratio = device as f64 / client as f64;
    let sinc_len = 8 * (p.sinc_len as f32 / 8.0).ceil() as usize;
    let cutoff = if ratio >= 1.0 { p.f_cutoff } else { p.f_cutoff * ratio as f32 };
    interpolators(sinc_len, p.oversampling_factor, cutoff, p.window)
        .into_iter()
        .map(|(name, interpolator)| {
            let resampler = SincFixedOut::<f32>::new_with_interpolator(
                ratio,
                MAX_RATIO_RELATIVE,
                INTERPOLATION,
                interpolator,
                PERIOD,
                CHANNELS,
            )
            .unwrap();
            (name, resampler)
        })
        .collect()
}

/// A `freq` sine at `client` through `resampler`, channel 0, from the first
/// frame past the start-up transient; and that frame's index.
fn run(mut resampler: SincFixedOut<f32>, client: u32, device: u32, freq: f64, seconds: f64) -> (Vec<f32>, usize) {
    let skip = resampler.output_delay() + 4096;
    let total = skip + (device as f64 * seconds) as usize;
    let mut out: Vec<f32> = Vec::with_capacity(total + PERIOD);
    let mut outbuf = resampler.output_buffer_allocate(true);
    let mut chunk = vec![vec![0f32; resampler.input_frames_max()]; CHANNELS];
    let mut n_in: u64 = 0;
    while out.len() < total {
        let need = resampler.input_frames_next();
        for i in 0..need {
            let t = (n_in + i as u64) as f64 / client as f64;
            let v = (amplitude() * (2.0 * PI * freq * t).sin()) as f32;
            for channel in chunk.iter_mut() {
                channel[i] = v;
            }
        }
        n_in += need as u64;
        let slices: Vec<&[f32]> = chunk.iter().map(|c| &c[..need]).collect();
        let (_, produced) = resampler.process_into_buffer(&slices, &mut outbuf, None).unwrap();
        out.extend_from_slice(&outbuf[0][..produced]);
    }
    out.truncate(total);
    (out.split_off(skip), skip)
}

/// IEEE 1057's three-parameter fit at a known frequency, `y ≈ a cos + b sin + c`:
/// the gain in dB against the tone, and the residual's level below the fitted
/// tone in dB.
fn fit(ys: &[f32], n0: usize, freq: f64, device: u32) -> (f64, f64) {
    let w = 2.0 * PI * freq / device as f64;
    let mut m = [[0f64; 3]; 3];
    let mut r = [0f64; 3];
    for (k, &y) in ys.iter().enumerate() {
        let n = (k + n0) as f64;
        let v = [(w * n).cos(), (w * n).sin(), 1.0];
        for i in 0..3 {
            for j in 0..3 {
                m[i][j] += v[i] * v[j];
            }
            r[i] += y as f64 * v[i];
        }
    }
    let det = |m: [[f64; 3]; 3]| {
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    };
    let d = det(m);
    let mut s = [0f64; 3];
    for i in 0..3 {
        let mut mi = m;
        for j in 0..3 {
            mi[j][i] = r[j];
        }
        s[i] = det(mi) / d;
    }
    let (mut err, mut sig) = (0f64, 0f64);
    for (k, &y) in ys.iter().enumerate() {
        let n = (k + n0) as f64;
        let f = s[0] * (w * n).cos() + s[1] * (w * n).sin() + s[2];
        err += (y as f64 - f).powi(2);
        sig += (f - s[2]).powi(2);
    }
    (20.0 * ((s[0] * s[0] + s[1] * s[1]).sqrt() / amplitude()).log10(), 10.0 * (sig / err).log10())
}

/// 20 Hz, AES17's 997 Hz, and the passband's edge.
fn in_band(client: u32, device: u32) -> [f64; 3] {
    [20.0, 997.0, (PASS_EDGE * client.min(device) as f64).min(20_000.0)]
}

/// From the stop edge to 0.499 of the client's rate, a sixth of an octave
/// apart, where a client faster than the device has a stop band below its own
/// Nyquist; empty otherwise.
fn stop_band(client: u32, device: u32) -> Vec<f64> {
    let (lo, hi) = (STOP_EDGE * device as f64, 0.499 * client as f64);
    if client <= device || lo >= hi {
        return Vec::new();
    }
    let mut v = Vec::new();
    let mut f = lo;
    while f < hi {
        v.push(f);
        f *= 2f64.powf(1.0 / 6.0);
    }
    v.push(hi);
    v
}

#[test]
fn the_passband_is_flat_within_the_src4192_s_ripple() {
    for (client, device) in pairs() {
        for freq in in_band(client, device) {
            for (name, resampler) in explicit(client, device) {
                let (ys, n0) = run(resampler, client, device, freq, SECONDS);
                let (gain, _) = fit(&ys, n0, freq, device);
                assert!(
                    gain.abs() <= RIPPLE_DB,
                    "{name}: {client} -> {device} Hz at {freq:.1} Hz has a gain of {gain:+.5} dB"
                );
            }
        }
    }
}

#[test]
fn in_band_thd_n_is_125_db_below_the_tone() {
    for (client, device) in pairs() {
        for freq in in_band(client, device) {
            for (name, resampler) in explicit(client, device) {
                let (ys, n0) = run(resampler, client, device, freq, SECONDS);
                let (_, thd_n) = fit(&ys, n0, freq, device);
                assert!(
                    thd_n >= REJECT_DB,
                    "{name}: {client} -> {device} Hz at {freq:.1} Hz leaves THD+N {thd_n:.1} dB below the tone"
                );
            }
        }
    }
}

#[test]
fn the_stop_band_is_rejected_by_125_db() {
    for (client, device) in pairs() {
        for freq in stop_band(client, device) {
            for (name, resampler) in explicit(client, device) {
                let (ys, _) = run(resampler, client, device, freq, SECONDS);
                let rms = (ys.iter().map(|&y| (y as f64).powi(2)).sum::<f64>() / ys.len() as f64).sqrt();
                let rejected = -20.0 * (rms / (amplitude() / 2f64.sqrt())).log10();
                assert!(
                    rejected >= REJECT_DB,
                    "{name}: {client} -> {device} Hz rejects {freq:.1} Hz by {rejected:.1} dB"
                );
            }
        }
    }
}

#[test]
fn what_a_stream_runs_is_one_of_the_explicit_builds() {
    for (client, device) in pairs() {
        let (built, _) = run(build(client, device, PERIOD, CHANNELS), client, device, 997.0, 0.25);
        let same = explicit(client, device).into_iter().any(|(_, resampler)| {
            let (theirs, _) = run(resampler, client, device, 997.0, 0.25);
            theirs.len() == built.len() && theirs.iter().zip(&built).all(|(a, b)| a.to_bits() == b.to_bits())
        });
        assert!(same, "{client} -> {device} Hz: what a stream runs is none of the explicit builds");
    }
}
