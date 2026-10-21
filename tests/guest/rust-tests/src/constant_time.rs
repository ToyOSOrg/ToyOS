//! Fixed-versus-random timing of a secret routine (Reparaz, Balasch and
//! Verbauwhede, "Dude, is my code constant time?", DATE 2017), as
//! `issues/toyos-crypto-is-in-the-tree-on-par-with-ring-and-secure.md`
//! stage 4 states it: one class holds a fixed secret, the other a fresh
//! uniform one, the public inputs the same in both, and the class drawn at
//! random per measurement. Welch's t over the raw measurements and over each
//! set cropped at a percentile of a calibration run; a routine passes below
//! [`THRESHOLD`].
//!
//! Each routine's input is made before its clock starts, so only the routine
//! is timed. The verdicts are evidence, printed one line per routine; the
//! program fails only where its leaky control goes unseen, which makes every
//! verdict of that run worthless.

use std::hint::black_box;
use std::time::Instant;

/// Measurements per routine, behind those that warm it and calibrate the
/// crops.
pub const MEASUREMENTS: usize = 1_000_000;
const WARMUP: usize = 1_000;
const CALIBRATION: usize = 10_000;
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

/// One routine's verdict.
pub struct Verdict {
    pub max_t: f64,
}

/// Times `run` on `MEASUREMENTS` inputs from `make`, and prints its line:
/// the raw t, the largest |t| and the crop it was found at, and each
/// class's mean.
pub fn measure<I>(name: &str, rng: &mut Rng, mut make: impl FnMut(Class, &mut Rng) -> I, mut run: impl FnMut(I)) -> Verdict {
    let mut once = |rng: &mut Rng| {
        let class = if rng.next() & 1 == 0 { Class::Fixed } else { Class::Random };
        let input = make(class, rng);
        let start = Instant::now();
        run(black_box(input));
        (class, start.elapsed().as_nanos() as f64)
    };
    for _ in 0..WARMUP {
        once(rng);
    }
    let mut calibration: Vec<f64> = (0..CALIBRATION).map(|_| once(rng).1).collect();
    calibration.sort_by(f64::total_cmp);
    let crops: Vec<f64> = (0..CROPS)
        .map(|k| {
            let p = 1.0 - 0.5f64.powf(10.0 * (k + 1) as f64 / CROPS as f64);
            calibration[((p * CALIBRATION as f64) as usize).min(CALIBRATION - 1)]
        })
        .collect();
    let mut raw = Welch::default();
    let mut cropped = vec![Welch::default(); CROPS];
    for _ in 0..MEASUREMENTS {
        let (class, x) = once(rng);
        raw.push(class, x);
        for (set, &bound) in cropped.iter_mut().zip(&crops) {
            if x <= bound {
                set.push(class, x);
            }
        }
    }
    let (mut max_t, mut at) = (raw.t().abs(), "raw".to_string());
    for (k, set) in cropped.iter().enumerate() {
        if set.judged() && set.t().abs() > max_t {
            (max_t, at) = (set.t().abs(), format!("crop {k} (<= {} ns)", crops[k]));
        }
    }
    let verdict = if max_t < THRESHOLD { "pass" } else { "LEAKS" };
    println!(
        "ct {name}: {verdict} max |t| {max_t:.2} at {at}; raw t {:.2}; n {} fixed, {} random; mean {:.1} ns fixed, {:.1} ns random",
        raw.t(),
        raw.n[0],
        raw.n[1],
        raw.mean[0],
        raw.mean[1],
    );
    Verdict { max_t }
}

/// The control: a byte-at-a-time comparison that stops at the first
/// difference, the leak a tag check must not have. The fixed class equals
/// the expected bytes and compares all of them; a random one stops at once.
pub fn leaky_control(rng: &mut Rng) {
    const EXPECTED: [u8; 64] = [0; 64];
    let verdict = measure(
        "leaky control (an early-exit comparison)",
        rng,
        |class, rng| match class {
            Class::Fixed => EXPECTED,
            Class::Random => rng.array(),
        },
        |secret| {
            let mut equal = true;
            for (a, b) in secret.iter().zip(black_box(&EXPECTED)) {
                if black_box(*a) != *b {
                    equal = false;
                    break;
                }
            }
            black_box(equal);
        },
    );
    if verdict.max_t <= CONTROL {
        eprintln!("ct: the leaky control reached only |t| {:.2}, not {CONTROL}: this run sees no leak", verdict.max_t);
        std::process::exit(1);
    }
}
