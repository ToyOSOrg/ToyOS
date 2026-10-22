//! Fixed-versus-random timing of a secret routine (Reparaz, Balasch and
//! Verbauwhede, "Dude, is my code constant time?", DATE 2017), as
//! `issues/toyos-crypto-is-in-the-tree-on-par-with-ring-and-secure.md`
//! stage 4 states it: one class holds a fixed secret, the other a fresh
//! uniform one, the public inputs the same in both, and the class drawn at
//! random per measurement. Welch's t over the raw measurements and over each
//! set cropped at a percentile of a calibration run; a routine passes below
//! [`THRESHOLD`].
//!
//! Inputs are made a [`BLOCK`] at a time before any of them is timed, and a
//! maker does the same work for either class, so what runs between two
//! counter reads is the routine alone, and nothing next to the clock
//! depends on the class. Each run has two controls: a [`null_control`],
//! whose classes are one distribution and whose |t| must stay below
//! [`THRESHOLD`], and a [`leaky_control`], a 16-byte early-exit comparison
//! that must exceed [`CONTROL`]. A run in which either fails exits nonzero,
//! since its verdicts cannot be read; every verdict is printed regardless.

use std::hint::black_box;

use crate::counter::{start, stop};

/// Measurements per routine, behind those that warm it and calibrate the
/// crops.
pub const MEASUREMENTS: usize = 1_000_000;
const WARMUP: usize = 1_000;
const CALIBRATION: usize = 10_000;
/// Inputs made ahead of their measurements, at a time.
pub const BLOCK: usize = 1_000;
/// The crops: dudect's 100 percentiles, 1 - 0.5^(10(k+1)/100), dense near
/// the top where a leak in a slow tail hides.
const CROPS: usize = 100;
/// A set is judged only with this many measurements of each class.
const LEAST: f64 = 1_000.0;
/// The pass bound on max |t| (dudect's, and the issue's).
pub const THRESHOLD: f64 = 4.5;
/// What the leaky control must exceed.
pub const CONTROL: f64 = 10.0;

/// SplitMix64: the class draw and the random class's secrets.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    pub fn array<const N: usize>(&mut self) -> [u8; N] {
        core::array::from_fn(|_| self.next() as u8)
    }
}

/// The fixed class or the random one.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Fixed,
    Random,
}

/// `fixed` for the fixed class, uniform bytes for the random one: drawn for
/// both, so that making either costs the same.
pub fn secret<const N: usize>(class: Class, rng: &mut Rng, fixed: [u8; N]) -> [u8; N] {
    let random = rng.array();
    match class {
        Class::Fixed => fixed,
        Class::Random => random,
    }
}

/// Welford's running mean and variance, per class.
#[derive(Clone, Copy, Default)]
struct Welch {
    n: [f64; 2],
    mean: [f64; 2],
    m2: [f64; 2],
}

impl Welch {
    fn push(&mut self, class: Class, x: f64) {
        let c = class as usize;
        self.n[c] += 1.0;
        let delta = x - self.mean[c];
        self.mean[c] += delta / self.n[c];
        self.m2[c] += delta * (x - self.mean[c]);
    }
    fn judged(&self) -> bool {
        self.n[0] >= LEAST && self.n[1] >= LEAST
    }
    fn t(&self) -> f64 {
        let var = |c: usize| self.m2[c] / (self.n[c] - 1.0);
        (self.mean[0] - self.mean[1]) / (var(0) / self.n[0] + var(1) / self.n[1]).sqrt()
    }
}

/// `n` measurements of `run`, their inputs made a [`BLOCK`] ahead.
fn measured<I>(
    n: usize,
    rng: &mut Rng,
    make: &mut impl FnMut(Class, &mut Rng) -> I,
    run: &mut impl FnMut(I),
    mut record: impl FnMut(Class, f64),
) {
    let mut left = n;
    while left > 0 {
        let size = left.min(BLOCK);
        left -= size;
        let inputs: Vec<(Class, I)> = (0..size)
            .map(|_| {
                let class = if rng.next() & 1 == 0 { Class::Fixed } else { Class::Random };
                (class, make(class, rng))
            })
            .collect();
        for (class, input) in inputs {
            let input = black_box(input);
            let begun = start();
            run(input);
            record(class, stop().wrapping_sub(begun) as f64);
        }
    }
}

/// Times `run` on `MEASUREMENTS` inputs from `make` and prints its line: the
/// raw t, the largest |t| and the crop it was found at, and each class's
/// mean in ticks. Returns the largest |t|.
pub fn measure<I>(name: &str, rng: &mut Rng, mut make: impl FnMut(Class, &mut Rng) -> I, mut run: impl FnMut(I)) -> f64 {
    measured(WARMUP, rng, &mut make, &mut run, |_, _| {});
    let mut calibration = Vec::with_capacity(CALIBRATION);
    measured(CALIBRATION, rng, &mut make, &mut run, |_, x| calibration.push(x));
    calibration.sort_by(f64::total_cmp);
    let crops: Vec<f64> = (0..CROPS)
        .map(|k| {
            let p = 1.0 - 0.5f64.powf(10.0 * (k + 1) as f64 / CROPS as f64);
            calibration[((p * CALIBRATION as f64) as usize).min(CALIBRATION - 1)]
        })
        .collect();
    let mut raw = Welch::default();
    let mut cropped = vec![Welch::default(); CROPS];
    measured(MEASUREMENTS, rng, &mut make, &mut run, |class, x| {
        raw.push(class, x);
        for (set, &bound) in cropped.iter_mut().zip(&crops) {
            if x <= bound {
                set.push(class, x);
            }
        }
    });
    let (mut max_t, mut at) = (raw.t().abs(), "raw".to_string());
    for (k, set) in cropped.iter().enumerate() {
        if set.judged() && set.t().abs() > max_t {
            (max_t, at) = (set.t().abs(), format!("crop {k} (<= {} ticks)", crops[k]));
        }
    }
    let verdict = if max_t < THRESHOLD { "pass" } else { "LEAKS" };
    println!(
        "ct {name}: {verdict} max |t| {max_t:.2} at {at}; raw t {:.2}; n {} fixed, {} random; mean {:.1} ticks fixed, {:.1} random",
        raw.t(),
        raw.n[0],
        raw.n[1],
        raw.mean[0],
        raw.mean[1],
    );
    max_t
}

const CONTROL_KEY: [u8; 32] = [0x6b; 32];
const CONTROL_IV: [u8; 12] = [0x24; 12];

/// A 64-byte ChaCha20-Poly1305 seal under the public control key: the work
/// both controls time, and the tag the leaky one compares.
fn sealed_tag() -> [u8; 16] {
    let mut data = [0x70; 64];
    let mut key = toyos_crypto::aead::SealingKey::chacha20_poly1305(CONTROL_KEY, CONTROL_IV);
    key.seal(&[], &mut data).unwrap().1
}

/// The same seal for both classes, whose received tags are uniform: one
/// distribution, so any |t| at or past [`THRESHOLD`] is the harness's own.
pub fn null_control(rng: &mut Rng) -> bool {
    let max_t = measure(
        "null control (one distribution in both classes)",
        rng,
        |_, rng| rng.array::<16>(),
        |received| {
            black_box((sealed_tag(), received));
        },
    );
    let held = max_t < THRESHOLD;
    if !held {
        eprintln!("ct: the null control reached |t| {max_t:.2}, past {THRESHOLD}: this run's verdicts cannot be read");
    }
    held
}

/// The seal, then the leak a tag check must not have: a comparison that stops
/// at the first differing byte. The fixed class's received tag is the
/// expected one with its last byte changed, so 15 bytes agree; a random
/// one's differs at its first byte but one time in 256.
pub fn leaky_control(rng: &mut Rng) -> bool {
    let mut forged = sealed_tag();
    forged[15] ^= 1;
    let max_t = measure(
        "leaky control (a 16-byte early-exit tag comparison)",
        rng,
        |class, rng| secret(class, rng, forged),
        |received| {
            let expected = sealed_tag();
            let mut equal = true;
            for (a, b) in expected.iter().zip(black_box(&received)) {
                if a != b {
                    equal = false;
                    break;
                }
            }
            black_box(equal);
        },
    );
    let seen = max_t > CONTROL;
    if !seen {
        eprintln!("ct: the leaky control reached only |t| {max_t:.2}, not {CONTROL}: this run sees no leak");
    }
    seen
}

/// Ends the run: nonzero where a control failed.
pub fn finish(controls_held: bool) {
    if !controls_held {
        std::process::exit(1);
    }
}
