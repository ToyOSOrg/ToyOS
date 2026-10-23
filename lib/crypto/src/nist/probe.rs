//! PROBE (never lands): P-256 and P-384 inversion and whole-operation costs,
//! divsteps against the Fermat forms they replace, for a bench's Linux. Run
//! with `probe_timings --ignored --nocapture --test-threads=1`.

use super::*;
use core::hint::black_box;
use std::time::Instant;
use std::vec::Vec;

fn time<T>(name: &str, iterations: u32, mut f: impl FnMut() -> T) {
    let mut samples: Vec<f64> = (0..15)
        .map(|_| {
            let start = Instant::now();
            for _ in 0..iterations {
                black_box(f());
            }
            start.elapsed().as_nanos() as f64 / f64::from(iterations)
        })
        .collect();
    samples.sort_by(f64::total_cmp);
    std::println!("probe {name} median={:.1}ns min={:.1}ns", samples[7], samples[0]);
}

fn square_times<M: Modulus<N>, const N: usize>(x: &Residue<M, N>, k: u32) -> Residue<M, N> {
    let mut r = *x;
    for _ in 0..k {
        r = r.square();
    }
    r
}

/// The removed runtime P-256 field chain: 255 squarings, 12 multiplications.
fn fermat_p256(x: &Residue<P256Field, 4>) -> Residue<P256Field, 4> {
    let s = square_times::<P256Field, 4>;
    let x2 = s(x, 1).mul(x);
    let x3 = s(&x2, 1).mul(x);
    let x6 = s(&x3, 3).mul(&x3);
    let x12 = s(&x6, 6).mul(&x6);
    let x15 = s(&x12, 3).mul(&x3);
    let x30 = s(&x15, 15).mul(&x15);
    let x32 = s(&x30, 2).mul(&x2);
    let r = s(&x32, 32).mul(x);
    let r = s(&r, 128).mul(&x32);
    let r = s(&r, 32).mul(&x32);
    let r = s(&r, 30).mul(&x30);
    s(&r, 2).mul(x)
}

/// The removed runtime generic form: 4-bit windows of p - 2.
fn fermat_windows<M: Modulus<N>, const N: usize>(x: &Residue<M, N>) -> Residue<M, N> {
    let mut e = M::P;
    e[0] -= 2;
    let mut powers = [Residue::<M, N>::one(); 16];
    for j in 1..16 {
        powers[j] = powers[j - 1].mul(x);
    }
    let mut r = Residue::one();
    for nibble in (0..N * 16).rev() {
        r = square_times(&r, 4);
        let digit = (e[nibble / 16] >> (4 * (nibble % 16))) & 15;
        if digit != 0 {
            r = r.mul(&powers[digit as usize]);
        }
    }
    r
}

#[test]
#[ignore]
fn probe_timings() {
    type F = Residue<P256Field, 4>;
    type P = Point<P256, 4>;
    let path = crate::arch::p256_mul(&[1, 0, 0, 0], &[1, 0, 0, 0]).is_some();
    std::println!("probe path {}", if path { "bmi2-adx" } else { "portable" });
    let x = F::from_montgomery(P256::GX);
    assert_eq!(fermat_p256(&x).integer(), x.invert().integer());
    time("field/invert-fermat", 2_000, || fermat_p256(&black_box(x)));
    time("field/invert-divsteps", 2_000, || black_box(x).invert());

    type N = ModN<P256, 4>;
    let s = N::from_wide(&[0x1111, 0x2222, 0x3333, 0x4444]);
    assert_eq!(fermat_windows(&s).integer(), s.invert().integer());
    time("order/invert-fermat", 2_000, || fermat_windows(&black_box(s)));
    time("order/invert-divsteps", 2_000, || black_box(s).invert());

    type F384 = Residue<P384Field, 6>;
    let x384 = F384::from_montgomery(P384::GX);
    assert_eq!(fermat_windows(&x384).integer(), x384.invert().integer());
    time("p384-field/invert-fermat", 1_000, || fermat_windows(&black_box(x384)));
    time("p384-field/invert-divsteps", 1_000, || black_box(x384).invert());
    type N384 = ModN<P384, 6>;
    let s384 = N384::from_wide(&[0x1111, 0x2222, 0x3333, 0x4444, 0x5555, 0x6666]);
    assert_eq!(fermat_windows(&s384).integer(), s384.invert().integer());
    time("p384-order/invert-fermat", 1_000, || fermat_windows(&black_box(s384)));
    time("p384-order/invert-divsteps", 1_000, || black_box(s384).invert());

    let k = Scalar::<P256, 4>::from_bytes(&[0x5c; 32]).unwrap();
    let q = P::base().mul(&k);
    let mut encoded = [0u8; 65];
    q.encode(&mut encoded).unwrap();
    time("mul/base_comb", 500, || P256::base_mul(black_box(&k)));
    time("mul/variable", 200, || black_box(q).mul(black_box(&k)));
    time("point/encode(affine)", 2_000, || {
        let mut out = [0u8; 65];
        black_box(q).encode(&mut out).unwrap();
        out[1]
    });
    time("ecdh/whole", 200, || {
        let public = P256::base_mul(black_box(&k));
        let mut out = [0u8; 65];
        public.encode(&mut out).unwrap();
        let peer = P::decode(black_box(&encoded)).unwrap();
        let mut secret = [0u8; 32];
        peer.agree(&k, &mut secret);
        (out[1], secret[0])
    });
    let u1 = PublicScalar([0x0123_4567_89ab_cdef, 0xfedc_ba98_7654_3210, 0x0f0f_0f0f_f0f0_f0f0, 0x7777_8888_9999_aaaa]);
    let u2 = PublicScalar([0x1357_9bdf_2468_ace0, 0x0fed_cba9_8765_4321, 0xaaaa_5555_aaaa_5555, 0x6666_1111_2222_3333]);
    time("verify/joint", 200, || black_box(q).joint(&u1, &u2));
    // A well-formed signature that does not verify: all of the work.
    let mut der = [0u8; 72];
    der[..2].copy_from_slice(&[0x30, 68]);
    der[2..4].copy_from_slice(&[0x02, 32]);
    der[4..36].copy_from_slice(&[0x3a; 32]);
    der[36..38].copy_from_slice(&[0x02, 32]);
    der[38..70].copy_from_slice(&[0x45; 32]);
    let der = &der[..70];
    assert!(q.verify(&[0x99; 32], der).is_err());
    time("verify/whole", 200, || black_box(q).verify(black_box(&[0x99; 32]), black_box(der)).is_err());
}

#[test]
#[ignore]
fn probe_divsteps_variants() {
    use crate::montgomery::divsteps;
    type N = ModN<P256, 4>;
    let s = N::from_wide(&[0x1111, 0x2222, 0x3333, 0x4444]);
    let raw = [0x1111u64, 0x2222, 0x3333, 0x4444];
    assert_eq!(divsteps::invert::<P256Order, 4>(&raw, &P256Order::R2), divsteps::invert_early::<P256Order, 4>(&raw, &P256Order::R2));
    time("order/invert-divsteps", 2_000, || black_box(s).invert());
    time("order/invert-divsteps-early", 2_000, || divsteps::invert_early::<P256Order, 4>(black_box(&raw), &P256Order::R2));
    // 2^255 - 19: divsteps against the 5x51 field's Fermat inversion.
    let bytes = [0x5au8; 32];
    let t = crate::field25519::Tight::from_bytes(&bytes);
    let mut w = [0u64; 4];
    for i in 0..4 {
        w[i] = u64::from_le_bytes(bytes[8 * i..8 * i + 8].try_into().unwrap());
    }
    let ours = divsteps::invert::<divsteps::P25519, 4>(&w, &[1, 0, 0, 0]);
    let mut ours_bytes = [0u8; 32];
    for i in 0..4 {
        ours_bytes[8 * i..8 * i + 8].copy_from_slice(&ours[i].to_le_bytes());
    }
    assert_eq!(ours_bytes, t.invert().bytes());
    time("f25519/invert-fermat", 2_000, || black_box(t).invert());
    time("f25519/invert-divsteps", 2_000, || divsteps::invert::<divsteps::P25519, 4>(black_box(&w), &[1, 0, 0, 0]));
}
