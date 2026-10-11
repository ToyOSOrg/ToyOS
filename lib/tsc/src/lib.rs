//! The time-stamp counter's rate as the part states it, read by the kernel and
//! the loader alike. Pure: the caller executes CPUID and hands over the
//! registers.

#![no_std]

/// The registers CPUID returns for one leaf: EAX, EBX, ECX, EDX.
pub type Leaf = (u32, u32, u32, u32);

/// SDM Vol. 2A, CPUID leaf 15H: EAX is the denominator and EBX the numerator of
/// the core crystal's ratio to the TSC, ECX the crystal's hertz — any of the
/// three reading zero means the leaf states nothing. Leaf 16H's EAX is the
/// processor base frequency in MHz, which an invariant TSC counts at. `None`
/// for a leaf the CPU does not implement, and as the answer where neither
/// states a rate: nothing here guesses one.
pub const fn stated_hz(leaf15: Option<Leaf>, leaf16: Option<Leaf>) -> Option<u64> {
    if let Some((denominator, numerator, crystal_hz, _)) = leaf15
        && denominator != 0
        && numerator != 0
        && crystal_hz != 0
    {
        return Some(crystal_hz as u64 * numerator as u64 / denominator as u64);
    }
    if let Some((base_mhz, _, _, _)) = leaf16
        && base_mhz != 0
    {
        return Some(base_mhz as u64 * 1_000_000);
    }
    None
}

const _: () = {
    // The ratio, then the fall-through to the base frequency when the crystal
    // is not enumerated, then the CPU that states neither.
    assert!(matches!(stated_hz(Some((2, 4, 25_000_000, 0)), None), Some(50_000_000)));
    assert!(matches!(stated_hz(Some((0, 0, 0, 0)), Some((2_400, 0, 0, 0))), Some(2_400_000_000)));
    assert!(stated_hz(None, Some((0, 0, 0, 0))).is_none());
    assert!(stated_hz(None, None).is_none());
};
