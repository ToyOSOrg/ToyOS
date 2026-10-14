//! The vector and arithmetic extensions a program's own feature detection
//! admits: CPUID gated by OSXSAVE and XCR0, as std reads them.

#[cfg(target_arch = "x86_64")]
pub fn detected() -> Vec<&'static str> {
    // `tt`, not `literal`: std's macro matches each feature's own token, which
    // a `literal` fragment hides.
    macro_rules! detected {
        ($($feature:tt),*) => {
            [$(($feature, std::is_x86_feature_detected!($feature))),*]
        };
    }
    let all = detected!(
        "aes", "pclmulqdq", "sha", "movbe", "bmi2", "adx", "avx", "avx2", "vaes", "vpclmulqdq", "avx512f", "avx512vl"
    );
    all.into_iter().filter(|(_, on)| *on).map(|(name, _)| name).collect()
}
