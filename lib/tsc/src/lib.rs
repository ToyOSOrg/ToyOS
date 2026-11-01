//! The time-stamp counter's rate as the part states it, read by the kernel and
//! the loader alike. Pure: the caller executes CPUID and hands over the
//! registers.

#![no_std]

/// The registers CPUID returns for one leaf: EAX, EBX, ECX, EDX.
pub type Leaf = (u32, u32, u32, u32);

/// SDM Vol. 2A, CPUID leaf 15H: EAX is the denominator and EBX the numerator of
/// the TSC's ratio to the core crystal, ECX the crystal's hertz, and the TSC
/// counts at their product; any of the three reading zero states no rate.
/// `None` for a CPU without the leaf. Leaf 16H's base frequency is no rate:
/// the SDM confines its data to display.
pub const fn stated_hz(leaf15: Option<Leaf>) -> Option<u64> {
    match leaf15 {
        Some((denominator, numerator, crystal_hz, _)) if denominator != 0 && numerator != 0 && crystal_hz != 0 => {
            Some(crystal_hz as u64 * numerator as u64 / denominator as u64)
        }
        _ => None,
    }
}

const _: () = {
    // A 38.4 MHz crystal at 63/1.
    assert!(matches!(stated_hz(Some((1, 63, 38_400_000, 0))), Some(2_419_200_000)));
    assert!(matches!(stated_hz(Some((2, 4, 25_000_000, 0))), Some(50_000_000)));
    // A crystal the leaf does not enumerate states nothing.
    assert!(stated_hz(Some((2, 176, 0, 0))).is_none());
    assert!(stated_hz(None).is_none());
};
