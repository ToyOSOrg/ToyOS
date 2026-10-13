//! The interpolators rubato can take on an x86-64 CPU ToyOS runs on: ToyOS
//! requires AVX, and every CPU with it has SSE3.

use rubato::sinc_interpolator::sinc_interpolator_avx::AvxInterpolator;
use rubato::sinc_interpolator::sinc_interpolator_sse::SseInterpolator;
use rubato::sinc_interpolator::SincInterpolator;
use rubato::WindowFunction;

/// Each by name, built with the sinc length and cutoff rubato's own dispatch
/// derives. A CPU that cannot run one panics, naming the feature it lacks.
pub(crate) fn interpolators(
    sinc_len: usize,
    oversampling: usize,
    cutoff: f32,
    window: WindowFunction,
) -> Vec<(&'static str, Box<dyn SincInterpolator<f32>>)> {
    vec![
        (
            "SseInterpolator",
            Box::new(
                SseInterpolator::<f32>::new(sinc_len, oversampling, cutoff, window)
                    .unwrap_or_else(|e| panic!("SseInterpolator: {e:?}")),
            ),
        ),
        (
            "AvxInterpolator",
            Box::new(
                AvxInterpolator::<f32>::new(sinc_len, oversampling, cutoff, window)
                    .unwrap_or_else(|e| panic!("AvxInterpolator: {e:?}")),
            ),
        ),
    ]
}
