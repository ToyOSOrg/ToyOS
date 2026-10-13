//! The resampler a client at another rate than the device's is carried by:
//! `rubato`'s windowed sinc, with the parameters its two rates take.
//!
//! Held to the dithered 16-bit wire by published practice, on every
//! interpolator rubato can take on a CPU ToyOS runs on (`tests.rs`):
//! - the passband, 20 Hz to 0.4535 of the lower rate and at most 20 kHz,
//!   within ±0.007 dB: the TI SRC4192/SRC4193 datasheet's (SBFS022C)
//!   interpolation filter;
//! - the stop band, from 0.5465 of the device rate when a client is faster,
//!   and in-band THD+N, each 125 dB below the tone: SoX's `rate` at its
//!   "high" quality, its figure for 16-bit mastering with dither.
//!
//! **The parameters.** A 512-tap Blackman-Harris² sinc, oversampled 256
//! times and interpolated cubically, cut off where rubato's
//! `calculate_cutoff` puts a 512-tap filter. Below a ratio of 1 rubato scales
//! the cutoff by the ratio and keeps the sinc's length in input frames, which
//! widens the transition against the output's Nyquist by the ratio; so a
//! client faster than the device lengthens the sinc by the ratio, rounded up
//! to a multiple of 8, which gives the 512-tap filter's response against the
//! lower rate, and divides the oversampling by it, rounded up, which keeps the
//! table at 512 KiB.

use rubato::{
    calculate_cutoff, SincFixedOut, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};

const SINC_LEN: usize = 512;
const OVERSAMPLING: usize = 256;
const WINDOW: WindowFunction = WindowFunction::BlackmanHarris2;
const INTERPOLATION: SincInterpolationType = SincInterpolationType::Cubic;

/// How far the ratio may move from its nominal value; soundserver never moves
/// it, and rubato sizes its buffers by it.
const MAX_RATIO_RELATIVE: f64 = 2.0;

/// The parameters a client at `client_rate` on a device at `device_rate` takes.
fn parameters(client_rate: u32, device_rate: u32) -> SincInterpolationParameters {
    let faster = (client_rate as f64 / device_rate as f64).max(1.0);
    let sinc_len = 8 * ((SINC_LEN as f64 * faster / 8.0).ceil() as usize);
    SincInterpolationParameters {
        sinc_len,
        f_cutoff: calculate_cutoff::<f32>(SINC_LEN, WINDOW),
        interpolation: INTERPOLATION,
        oversampling_factor: (OVERSAMPLING as f64 / faster).ceil() as usize,
        window: WINDOW,
    }
}

/// The resampler for one stream: `client_rate` in, `device_rate` out in
/// periods of `period_frames`, over `channels`.
pub fn build(
    client_rate: u32,
    device_rate: u32,
    period_frames: usize,
    channels: usize,
) -> SincFixedOut<f32> {
    SincFixedOut::<f32>::new(
        device_rate as f64 / client_rate as f64,
        MAX_RATIO_RELATIVE,
        parameters(client_rate, device_rate),
        period_frames,
        channels,
    )
    .expect("the parameters are this module's and every ratio between accepted rates is positive")
}

#[cfg(test)]
mod interpolators;
#[cfg(test)]
mod tests;
