//! Every interpolator rubato can take on a CPU ToyOS runs on, by
//! architecture: the selector the resampler's tests build each one through.

#[cfg(target_arch = "x86_64")]
#[path = "x86_64.rs"]
mod arch;
#[cfg(target_arch = "aarch64")]
#[path = "aarch64.rs"]
mod arch;

pub(crate) use arch::interpolators;
