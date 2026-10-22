//! Width-w non-adjacent forms of public scalars, for variable-time
//! verification (Hankerson, Menezes and Vanstone, Guide to Elliptic Curve
//! Cryptography, algorithm 3.35).

/// The width-w NAF of k into `digits`, w in 2..=8: odd digits below 2^(w-1)
/// in magnitude, any two nonzero ones at least w positions apart. k must be
/// below 2^(64N) - 2^7, so that adding a digit never carries out, and
/// `digits` must be at least one longer than k's bit length. Variable time:
/// k is public.
pub(crate) fn naf<const N: usize>(k: &[u64; N], w: u32, digits: &mut [i8]) {
    debug_assert!((2..=8).contains(&w));
    let mut k = *k;
    digits.fill(0);
    let mut i = 0;
    while k != [0; N] {
        if k[0] & 1 == 1 {
            let window = (k[0] & ((1 << w) - 1)) as i32;
            let d = if window >= 1 << (w - 1) { window - (1 << w) } else { window };
            digits[i] = d as i8;
            if d >= 0 {
                k[0] -= d as u64;
            } else {
                let (s, mut carry) = k[0].overflowing_add((-d) as u64);
                k[0] = s;
                for word in &mut k[1..] {
                    if !carry {
                        break;
                    }
                    (*word, carry) = word.overflowing_add(1);
                }
            }
        }
        for j in 0..N - 1 {
            k[j] = k[j] >> 1 | k[j + 1] << 63;
        }
        k[N - 1] >>= 1;
        i += 1;
    }
}
