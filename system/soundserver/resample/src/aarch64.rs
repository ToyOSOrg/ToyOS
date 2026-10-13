//! The interpolator rubato can take on an AArch64 CPU ToyOS runs on: the
//! target compiles NEON in.

use rubato::sinc_interpolator::sinc_interpolator_neon::NeonInterpolator;
use rubato::sinc_interpolator::SincInterpolator;
use rubato::WindowFunction;

/// Built with the sinc length and cutoff rubato's own dispatch derives.
pub(crate) fn interpolators(
    sinc_len: usize,
    oversampling: usize,
    cutoff: f32,
    window: WindowFunction,
) -> Vec<(&'static str, Box<dyn SincInterpolator<f32>>)> {
    vec![(
        "NeonInterpolator",
        Box::new(
            NeonInterpolator::<f32>::new(sinc_len, oversampling, cutoff, window)
                .unwrap_or_else(|e| panic!("NeonInterpolator: {e:?}")),
        ),
    )]
}
