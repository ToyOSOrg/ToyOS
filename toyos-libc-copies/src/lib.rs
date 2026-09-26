//! libc's architecture module on the host, differentially: every copy and fill
//! it has, over every length to 300 and every source and destination offset to
//! 20, against `copy_within` and `fill`, with the two buffers overlapping both
//! ways; and its square roots against `f64::sqrt` and `f32::sqrt`. Each host
//! architecture checks its own module.

#[cfg(test)]
#[path = "../../userland/libc/src/arch/mod.rs"]
mod arch;

#[cfg(test)]
mod tests {
    use super::arch;

    const LENGTHS: usize = 300;
    const OFFSETS: usize = 20;

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i.wrapping_mul(31) ^ (i >> 3)) as u8).collect()
    }

    /// One buffer, so the copy's two ends can overlap by any amount either way.
    #[test]
    fn every_copy_agrees_with_copy_within() {
        let mut cases = 0u32;
        for n in 0..LENGTHS {
            for from in 0..OFFSETS {
                for to in 0..OFFSETS {
                    let base = pattern(n + 2 * OFFSETS);
                    let mut want = base.clone();
                    want.copy_within(from..from + n, to);
                    let mut got = base.clone();
                    let p = got.as_mut_ptr();
                    // SAFETY: both ranges are inside `got`, and the direction is
                    // the one each overlap needs: forward when the destination
                    // is below the source, backward when above.
                    unsafe {
                        if to <= from {
                            arch::copy_forward(p.add(to), p.add(from), n);
                        } else {
                            arch::copy_backward(p.add(to), p.add(from), n);
                        }
                    }
                    assert_eq!(got, want, "n {n} from {from} to {to}");
                    cases += 1;
                }
            }
        }
        assert_eq!(cases, (LENGTHS * OFFSETS * OFFSETS) as u32);
    }

    /// Two buffers, so neither direction leans on the other's order.
    #[test]
    fn a_disjoint_copy_agrees_either_way() {
        for n in 0..LENGTHS {
            for at in 0..OFFSETS {
                let src = pattern(n + OFFSETS);
                for backward in [false, true] {
                    let mut got = vec![0xAAu8; n + OFFSETS];
                    // SAFETY: `n` bytes at `at` in each of two buffers of `n + OFFSETS`.
                    unsafe {
                        let (d, s) = (got.as_mut_ptr().add(at), src.as_ptr().add(at));
                        if backward {
                            arch::copy_backward(d, s, n);
                        } else {
                            arch::copy_forward(d, s, n);
                        }
                    }
                    let mut want = vec![0xAAu8; n + OFFSETS];
                    want[at..at + n].copy_from_slice(&src[at..at + n]);
                    assert_eq!(got, want, "n {n} at {at} backward {backward}");
                }
            }
        }
    }

    #[test]
    fn every_fill_agrees_with_fill() {
        for n in 0..LENGTHS {
            for at in 0..OFFSETS {
                for byte in [0u8, 0x5A, 0xFF] {
                    let mut got = pattern(n + 2 * OFFSETS);
                    let mut want = got.clone();
                    want[at..at + n].fill(byte);
                    // SAFETY: `n` bytes at `at` inside `got`.
                    unsafe { arch::fill(got.as_mut_ptr().add(at), byte, n) };
                    assert_eq!(got, want, "n {n} at {at} byte {byte:#x}");
                }
            }
        }
    }

    #[test]
    fn the_square_roots_are_the_correctly_rounded_ones() {
        for x in [0.0f64, 1.0, 2.0, 0.5, 1e-300, 1e300, f64::MAX, f64::MIN_POSITIVE, 123456.789] {
            assert_eq!(arch::sqrt_f64(x).to_bits(), x.sqrt().to_bits(), "{x}");
            let y = x as f32;
            assert_eq!(arch::sqrt_f32(y).to_bits(), y.sqrt().to_bits(), "{y}");
        }
        assert!(arch::sqrt_f64(-1.0).is_nan() && arch::sqrt_f32(-1.0).is_nan());
    }
}
