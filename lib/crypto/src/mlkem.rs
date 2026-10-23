//! FIPS 203 ML-KEM-768, for TLS and SSH hybrid exchange. Secret polynomial
//! operations, CBD sampling, hashes and re-encryption have fixed schedules,
//! and no secret value is divided. Matrix rejection sampling depends only on the
//! public seed. Public-key decoding rejects coefficients >=3329. Decapsulation
//! implements masked implicit rejection, with no validity bit or error oracle.
//! Secret keys can only originate from the caller's entropy source, so external
//! expanded decapsulation-key validation is outside this API.
//!
//! Coefficients are signed 16-bit representatives, reduced only where a bound
//! requires it (Seiler, IACR ePrint 2018/039). The reductions, transforms and
//! products state their input bounds.

use crate::{
    Error, Random, ct,
    keccak::{self, Sponge},
    secret::{Secret, wipe},
};
const Q: i16 = 3329;
/// q⁻¹ mod 2^16.
const Q_INVERSE: i16 = -3327;
const _: () = assert!(Q.wrapping_mul(Q_INVERSE) == 1);
/// 2^16 mod q, the Montgomery factor.
const R: i32 = (1 << 16) % Q as i32;
/// R² mod q: a Montgomery product by it removes one factor 2^-16.
const R_SQUARED: i16 = (R * R % Q as i32) as i16;
/// R²/128 mod q (3303 is 128⁻¹), Algorithm 10's final scale with that removal.
const INVERSE_SCALE: i16 = (R * R % Q as i32 * 3303 % Q as i32) as i16;
const _: () = assert!(128 * 3303 % Q as i32 == 1);
struct Poly([i16; 256]);
impl Drop for Poly {
    fn drop(&mut self) {
        for word in &mut self.0 {
            // SAFETY: the address is a live, exclusively borrowed coefficient.
            unsafe { core::ptr::write_volatile(word, 0) }
        }
        core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst)
    }
}
/// ⌊n / q⌋ for n < 2^33 / 623, which holds every compressed value, by a
/// multiply and shift: a divide's latency can depend on its operands
/// (KyberSlash, Bernstein et al., IACR ePrint 2024/1049). With
/// M = ⌈2^33 / q⌉, n·M / 2^33 = n / q + 623·n / (q·2^33), and the excess is
/// below 1/q, too small to carry the quotient past the next integer.
fn divide(n: u32) -> u32 {
    const M: u64 = (1u64 << 33).div_ceil(Q as u64);
    ((u64::from(n) * M) >> 33) as u32
}
/// a·2^-16 mod q, in (-q, q), for |a| < q·2^15 (Montgomery reduction, in
/// Seiler's signed form): a − t·q is a multiple of 2^16 for t = a·q⁻¹ mod 2^16.
fn montgomery(a: i32) -> i16 {
    let t = (a as i16).wrapping_mul(Q_INVERSE);
    ((a - i32::from(t) * i32::from(Q)) >> 16) as i16
}
/// a·b·2^-16 mod q, in (-q, q), for |a·b| < q·2^15.
fn multiply(a: i16, b: i16) -> i16 {
    montgomery(i32::from(a) * i32::from(b))
}
/// a mod q, in [-(q-1)/2, (q-1)/2], for any a: a less q times the nearest
/// integer to a·⌊2^26/q⌉/2^26 (Barrett reduction).
fn barrett(a: i16) -> i16 {
    const V: i32 = ((1 << 26) + Q as i32 / 2) / Q as i32;
    let t = (V * i32::from(a) + (1 << 25)) >> 26;
    (i32::from(a) - t * i32::from(Q)) as i16
}
/// a mod q, in [0, q).
fn canonical(a: i16) -> i16 {
    let r = barrett(a);
    r + ((r >> 15) & Q)
}
/// ζ^BitRev7(i) and, for `odd`, γ_i = ζ^(2·BitRev7(i)+1), with ζ = 17: FIPS 203
/// Appendix A's two tables, each entry times R, which a Montgomery product
/// by it cancels.
const fn powers(odd: bool) -> [i16; 128] {
    let mut out = [0; 128];
    let mut i = 0;
    while i < 128 {
        let reversed = ((i as u8).reverse_bits() >> 1) as u32;
        let exponent = if odd { 2 * reversed + 1 } else { reversed };
        let mut power = R;
        let mut k = 0;
        while k < exponent {
            power = power * 17 % Q as i32;
            k += 1
        }
        out[i] = power as i16;
        i += 1
    }
    out
}
const ZETAS: [i16; 128] = powers(false);
const GAMMAS: [i16; 128] = powers(true);
impl Poly {
    fn zero() -> Self {
        Self([0; 256])
    }
    /// Algorithm 9, for coefficients below q in magnitude. A layer adds less
    /// than q to a magnitude, so seven stay below 8q < 2^15. The result is
    /// Barrett-reduced.
    fn ntt(mut self) -> Self {
        let mut zetas = ZETAS[1..].iter();
        for layer in (1..8).rev() {
            let len = 1 << layer;
            for (block, &z) in self.0.chunks_exact_mut(2 * len).zip(&mut zetas) {
                let (low, high) = block.split_at_mut(len);
                for (a, b) in low.iter_mut().zip(high) {
                    let t = multiply(z, *b);
                    *b = *a - t;
                    *a += t
                }
            }
        }
        for c in &mut self.0 {
            *c = barrett(*c)
        }
        self
    }
    /// Algorithm 10 of [`dot`]'s output, whose factor 2^-16 the final scale
    /// removes. Coefficients enter and stay below q in magnitude.
    fn inverse(mut self) -> Self {
        let mut zetas = ZETAS[1..].iter().rev();
        for layer in 1..8 {
            let len = 1 << layer;
            for (block, &z) in self.0.chunks_exact_mut(2 * len).zip(&mut zetas) {
                let (low, high) = block.split_at_mut(len);
                for (a, b) in low.iter_mut().zip(high) {
                    let t = *a;
                    *a = barrett(t + *b);
                    *b = multiply(z, *b - t)
                }
            }
        }
        self.scale(INVERSE_SCALE)
    }
    /// Each coefficient times factor·2^-16, in (-q, q).
    fn scale(mut self, factor: i16) -> Self {
        for c in &mut self.0 {
            *c = multiply(*c, factor)
        }
        self
    }
    fn plus(mut self, b: &Self) -> Self {
        for (c, b) in self.0.iter_mut().zip(&b.0) {
            *c += b
        }
        self
    }
    fn minus(mut self, b: &Self) -> Self {
        for (c, b) in self.0.iter_mut().zip(&b.0) {
            *c -= b
        }
        self
    }
    fn canonical(mut self) -> Self {
        for c in &mut self.0 {
            *c = canonical(*c)
        }
        self
    }
    /// ByteEncode_D (FIPS 203 Algorithm 5) of coefficients in [0, 2^D): each
    /// eight fill D bytes.
    fn encode<const D: usize>(&self, output: &mut [u8]) {
        assert_eq!(output.len(), 32 * D);
        for (coefficients, bytes) in self.0.as_chunks::<8>().0.iter().zip(output.as_chunks_mut::<D>().0) {
            let mut word = 0u128;
            for (i, &c) in coefficients.iter().enumerate() {
                word |= u128::from(c as u16) << (D * i)
            }
            bytes.copy_from_slice(&word.to_le_bytes()[..D])
        }
    }
    /// ByteDecode_D (Algorithm 6), whose coefficients are in [0, 2^D).
    fn decode<const D: usize>(input: &[u8]) -> Self {
        assert_eq!(input.len(), 32 * D);
        let mut out = Self::zero();
        for (coefficients, bytes) in out.0.as_chunks_mut::<8>().0.iter_mut().zip(input.as_chunks::<D>().0) {
            let mut word = [0; 16];
            word[..D].copy_from_slice(bytes);
            let word = u128::from_le_bytes(word);
            for (i, c) in coefficients.iter_mut().enumerate() {
                *c = (word >> (D * i)) as i16 & ((1 << D) - 1)
            }
        }
        out
    }
    /// Compress_d of any representatives, into [0, 2^d).
    fn compress(mut self, d: usize) -> Self {
        for c in &mut self.0 {
            let x = canonical(*c) as u32;
            *c = (divide((x << d) + Q as u32 / 2) & ((1 << d) - 1)) as i16
        }
        self
    }
    /// Decompress_d of [0, 2^d), into [0, q).
    fn decompress(mut self, d: usize) -> Self {
        for c in &mut self.0 {
            *c = ((Q as u32 * *c as u32 + (1 << (d - 1))) >> d) as i16
        }
        self
    }
}
/// Σ a_j·b_j in the NTT domain (Algorithms 11 and 12), times 2^-16, in
/// (-q, q). Each a_j's coefficients are below q in magnitude and each b_j's
/// at most (q-1)/2, so a pair's three products sum below 4.5q² < q·2^15 and
/// are reduced once.
fn dot(a: &[Poly; 3], b: &[Poly; 3]) -> Poly {
    let mut out = Poly::zero();
    for (i, &gamma) in GAMMAS.iter().enumerate() {
        let (mut even, mut odd) = (0, 0);
        for (a, b) in a.iter().zip(b) {
            let (a0, a1) = (i32::from(a.0[2 * i]), i32::from(a.0[2 * i + 1]));
            let (b0, b1) = (i32::from(b.0[2 * i]), i32::from(b.0[2 * i + 1]));
            even += a0 * b0 + i32::from(montgomery(a1 * b1)) * i32::from(gamma);
            odd += a0 * b1 + a1 * b0
        }
        out.0[2 * i] = montgomery(even);
        out.0[2 * i + 1] = montgomery(odd)
    }
    out
}
/// SamplePolyCBD_2 (Algorithm 8), in [-2, 2].
fn noise(seed: &[u8; 32], nonce: u8) -> Poly {
    let mut bytes = Secret([0; 128]);
    Sponge::new(136, 31, &[seed, &[nonce]]).read(&mut bytes.0);
    let mut out = Poly::zero();
    for i in 0..256 {
        let bits = (bytes.0[i / 2] >> (4 * (i % 2))) & 15;
        let a = (bits & 1) + ((bits >> 1) & 1);
        let b = ((bits >> 2) & 1) + ((bits >> 3) & 1);
        out.0[i] = i16::from(a) - i16::from(b)
    }
    out
}
/// SampleNTT (Algorithm 7), in [0, q).
fn sample(rho: &[u8; 32], row: usize, col: usize) -> Poly {
    let mut xof = Sponge::new(168, 31, &[rho, &[col as u8, row as u8]]);
    let mut out = Poly::zero();
    let mut pos = 0;
    let mut block = [0; 168];
    while pos < 256 {
        xof.read(&mut block);
        for b in block.as_chunks::<3>().0 {
            for v in [
                i16::from(b[0]) | ((i16::from(b[1]) & 15) << 8),
                (i16::from(b[1]) >> 4) | (i16::from(b[2]) << 4),
            ] {
                if v < Q && pos < 256 {
                    out.0[pos] = v;
                    pos += 1
                }
            }
        }
    }
    out
}
pub struct EncapsulationKey {
    bytes: [u8; 1184],
}
pub struct DecapsulationKey {
    /// ŝ, Barrett-reduced.
    secret: [Poly; 3],
    public: EncapsulationKey,
    hash: [u8; 32],
    rejection: Secret<32>,
}
pub struct SharedSecret(Secret<32>);
impl SharedSecret {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0.0
    }
}
impl EncapsulationKey {
    pub fn from_bytes(input: &[u8]) -> Result<Self, Error> {
        if input.len() != 1184 {
            return Err(Error::Length);
        }
        for chunk in input[..1152].as_chunks::<384>().0 {
            let p = Poly::decode::<12>(chunk);
            if p.0.iter().any(|&x| x >= Q) {
                return Err(Error::InvalidKey);
            }
        }
        Ok(Self {
            bytes: input.try_into().unwrap(),
        })
    }
    pub fn as_bytes(&self) -> &[u8; 1184] {
        &self.bytes
    }
    pub fn encapsulate(
        &self,
        random: &mut impl Random,
    ) -> Result<([u8; 1088], SharedSecret), Error> {
        let mut message = Secret([0; 32]);
        random.fill(&mut message.0)?;
        let hash = keccak::h(&[&self.bytes]);
        let kr = Secret(keccak::g(&[&message.0, &hash]));
        let ciphertext = self.encrypt(&message.0, kr.0[32..].try_into().unwrap());
        Ok((
            ciphertext,
            SharedSecret(Secret(kr.0[..32].try_into().unwrap())),
        ))
    }
    fn encrypt(&self, message: &[u8; 32], coins: &[u8; 32]) -> [u8; 1088] {
        let rho: &[u8; 32] = self.bytes[1152..].try_into().unwrap();
        let y: [Poly; 3] = core::array::from_fn(|i| noise(coins, i as u8).ntt());
        let mut out = [0; 1088];
        for i in 0..3 {
            let column: [Poly; 3] = core::array::from_fn(|j| sample(rho, j, i));
            dot(&column, &y)
                .inverse()
                .plus(&noise(coins, (i + 3) as u8))
                .compress(10)
                .encode::<10>(&mut out[i * 320..i * 320 + 320]);
        }
        let t: [Poly; 3] = core::array::from_fn(|j| Poly::decode::<12>(&self.bytes[j * 384..j * 384 + 384]));
        dot(&t, &y)
            .inverse()
            .plus(&noise(coins, 6))
            .plus(&Poly::decode::<1>(message).decompress(1))
            .compress(4)
            .encode::<4>(&mut out[960..]);
        out
    }
}
impl DecapsulationKey {
    pub fn generate(random: &mut impl Random) -> Result<Self, Error> {
        let mut seeds = Secret([0; 64]);
        random.fill(&mut seeds.0)?;
        let expanded = Secret(keccak::g(&[&seeds.0[..32], &[3]]));
        let rho: &[u8; 32] = expanded.0[..32].try_into().unwrap();
        let sigma: &[u8; 32] = expanded.0[32..].try_into().unwrap();
        let secret: [Poly; 3] = core::array::from_fn(|i| noise(sigma, i as u8).ntt());
        let mut bytes = [0; 1184];
        for i in 0..3 {
            let row: [Poly; 3] = core::array::from_fn(|j| sample(rho, i, j));
            dot(&row, &secret)
                .scale(R_SQUARED)
                .plus(&noise(sigma, (i + 3) as u8).ntt())
                .canonical()
                .encode::<12>(&mut bytes[i * 384..i * 384 + 384])
        }
        bytes[1152..].copy_from_slice(rho);
        let hash = keccak::h(&[&bytes]);
        Ok(Self {
            secret,
            public: EncapsulationKey { bytes },
            hash,
            rejection: Secret(seeds.0[32..].try_into().unwrap()),
        })
    }
    pub fn public_key(&self) -> &EncapsulationKey {
        &self.public
    }
    /// Invalid, correctly sized ciphertexts return a pseudorandom shared secret.
    /// The calling protocol discovers failure through its key confirmation.
    pub fn decapsulate(self, ciphertext: &[u8]) -> Result<SharedSecret, Error> {
        if ciphertext.len() != 1088 {
            return Err(Error::Length);
        }
        let u: [Poly; 3] =
            core::array::from_fn(|i| Poly::decode::<10>(&ciphertext[i * 320..i * 320 + 320]).decompress(10).ntt());
        let mut message = Secret([0; 32]);
        Poly::decode::<4>(&ciphertext[960..])
            .decompress(4)
            .minus(&dot(&self.secret, &u).inverse())
            .compress(1)
            .encode::<1>(&mut message.0);
        let kr = Secret(keccak::g(&[&message.0, &self.hash]));
        let rejected = Secret(keccak::j(&[&self.rejection.0, ciphertext]));
        let mut expected = self
            .public
            .encrypt(&message.0, kr.0[32..].try_into().unwrap());
        let mut difference = 0u64;
        for (&a, &b) in ciphertext.iter().zip(&expected) {
            difference |= u64::from(a ^ b)
        }
        wipe(&mut expected);
        let invalid = ct::nonzero(difference);
        let mut result = Secret([0; 32]);
        for i in 0..32 {
            result.0[i] = ct::select(u64::from(kr.0[i]), u64::from(rejected.0[i]), invalid) as u8
        }
        Ok(SharedSecret(result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;
    struct Input {
        bytes: Vec<u8>,
    }
    impl Random for Input {
        fn fill(&mut self, out: &mut [u8]) -> Result<(), Error> {
            out.copy_from_slice(&self.bytes[..out.len()]);
            Ok(())
        }
    }
    fn hex(s: &str) -> Vec<u8> {
        (0..s.len() / 2)
            .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
            .collect()
    }
    #[test]
    #[cfg_attr(miri, ignore = "13.8 M quotients")]
    fn division_by_q_is_exact_over_its_whole_input_range() {
        for n in 0..=((1u64 << 33) / 623) as u32 {
            assert_eq!(divide(n), n / Q as u32, "{n}");
        }
    }

    /// Each reduction over its whole input range, against 64-bit integers.
    #[test]
    #[cfg_attr(miri, ignore = "2^28 reductions")]
    fn reductions_are_exact_over_their_whole_input_ranges() {
        let q = i64::from(Q);
        for a in i16::MIN..=i16::MAX {
            let (r, c) = (barrett(a), canonical(a));
            assert!((i64::from(r) - i64::from(a)) % q == 0 && 2 * r.abs() < Q, "barrett({a}) = {r}");
            assert_eq!(i64::from(c), i64::from(a).rem_euclid(q), "canonical({a})");
        }
        let bound = i32::from(Q) << 15;
        for a in -bound + 1..bound {
            let r = montgomery(a);
            assert!((i64::from(r) * 65536 - i64::from(a)) % q == 0 && r.abs() < Q, "montgomery({a}) = {r}");
        }
    }

    /// x mod (X² − γ_i) for each i, the NTT's definition (FIPS 203 §4.3), with
    /// γ_i = 17^(2·BitRev7(i)+1), in plain 64-bit arithmetic.
    fn ntt_by_definition(x: &[i16; 256]) -> [i64; 256] {
        let q = i64::from(Q);
        let mut out = [0; 256];
        for i in 0..128 {
            let gamma = (0..2 * ((i as u8).reverse_bits() >> 1) as u32 + 1).fold(1, |p, _| p * 17 % q);
            let mut power = 1;
            for m in 0..128 {
                out[2 * i] = (out[2 * i] + i64::from(x[2 * m]) * power).rem_euclid(q);
                out[2 * i + 1] = (out[2 * i + 1] + i64::from(x[2 * m + 1]) * power).rem_euclid(q);
                power = power * gamma % q
            }
        }
        out
    }

    /// The lazily reduced transforms at the edges of their stated input
    /// bounds, where an overflow would be, against the definition.
    #[test]
    fn the_ntt_domain_holds_its_bounds_and_the_definition() {
        let q = i64::from(Q);
        let r = (1i64 << 16) % q;
        let patterns: [fn(usize, i16) -> i16; 4] =
            [|_, m| m, |_, m| -m, |i, m| if i % 2 == 0 { m } else { -m }, |i, m| (i as i16 * 97 % (2 * m + 1)) - m];
        for pattern in patterns {
            let wide = Poly(core::array::from_fn(|i| pattern(i, Q - 1)));
            let narrow = Poly(core::array::from_fn(|i| pattern(i, (Q - 1) / 2)));

            let want = ntt_by_definition(&wide.0);
            let ntt = Poly(wide.0).ntt();
            for (i, &c) in ntt.0.iter().enumerate() {
                assert!(2 * c.abs() < Q && i64::from(canonical(c)) == want[i], "ntt[{i}] = {c}");
            }

            let inverse = Poly(wide.0).inverse();
            let back = ntt_by_definition(&inverse.0);
            for (i, &c) in inverse.0.iter().enumerate() {
                assert!(c.abs() < Q && back[i] == (i64::from(wide.0[i]) * r).rem_euclid(q), "inverse[{i}] = {c}");
            }

            let (a, b) = ([Poly(wide.0), Poly(wide.0), Poly(wide.0)], [Poly(narrow.0), Poly(narrow.0), Poly(narrow.0)]);
            let product = dot(&a, &b);
            for i in 0..128 {
                let gamma = (0..2 * ((i as u8).reverse_bits() >> 1) as u32 + 1).fold(1, |p, _| p * 17 % q);
                let (a0, a1) = (i64::from(wide.0[2 * i]), i64::from(wide.0[2 * i + 1]));
                let (b0, b1) = (i64::from(narrow.0[2 * i]), i64::from(narrow.0[2 * i + 1]));
                for (k, want) in [3 * (a0 * b0 + a1 * b1 % q * gamma), 3 * (a0 * b1 + a1 * b0)].into_iter().enumerate() {
                    let c = product.0[2 * i + k];
                    assert!(c.abs() < Q && (i64::from(c) * 65536 - want) % q == 0, "dot[{}] = {c}", 2 * i + k);
                }
            }
        }
    }

    /// FIPS 203 §4.2.1's rounding, for each width ML-KEM-768 uses, over
    /// every input: Compress_d(x) = ⌈(2^d / q)·x⌋ mod 2^d and
    /// Decompress_d(y) = ⌈(q / 2^d)·y⌋, ties rounding up.
    #[test]
    fn compression_rounds_as_fips_203_for_every_input() {
        for d in [1, 4, 10] {
            let mut x = Poly::zero();
            for start in (0..Q).step_by(256) {
                for (i, word) in x.0.iter_mut().enumerate() {
                    *word = (start + i as i16).min(Q - 1);
                }
                let compressed = Poly(x.0).compress(d);
                for (i, &value) in x.0.iter().enumerate() {
                    let want = ((value as u64) << (d + 1)) + Q as u64;
                    let want = want / (2 * Q as u64) % (1 << d);
                    assert_eq!(compressed.0[i] as u64, want, "Compress_{d}({value})");
                }
            }
            let mut y = Poly::zero();
            for start in (0..1u32 << d).step_by(256) {
                for (i, word) in y.0.iter_mut().enumerate() {
                    *word = (start + i as u32).min((1 << d) - 1) as i16;
                }
                let decompressed = Poly(y.0).decompress(d);
                for (i, &value) in y.0.iter().enumerate() {
                    let want = (2 * Q as u64 * value as u64 + (1 << d)) >> (d + 1);
                    assert_eq!(decompressed.0[i] as u64, want, "Decompress_{d}({value})");
                }
            }
        }
    }

    /// NIST's keygen vectors are read, not embedded: they sit outside this
    /// crate's directory, as test input the image does not carry. Miri reads
    /// the file with its isolation off (`-Zmiri-disable-isolation`).
    #[test]
    fn nist_expanded_keygen_secret_matches() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/acvp/nist-mlkem-keygen.json");
        let data: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let mut count = 0;
        for group in data["testGroups"].as_array().unwrap() {
            if group["parameterSet"] != "ML-KEM-768" {
                continue;
            }
            // Miri interprets each key generation: one case there.
            for test in group["tests"].as_array().unwrap().iter().take(if cfg!(miri) { 1 } else { usize::MAX }) {
                let mut bytes = hex(test["d"].as_str().unwrap());
                bytes.extend(hex(test["z"].as_str().unwrap()));
                let key = DecapsulationKey::generate(&mut Input { bytes }).unwrap();
                let expected = hex(test["dk"].as_str().unwrap());
                let mut actual = Secret([0; 2400]);
                for (i, p) in key.secret.iter().enumerate() {
                    Poly(p.0).canonical().encode::<12>(&mut actual.0[384 * i..384 * i + 384])
                }
                actual.0[1152..2336].copy_from_slice(key.public.as_bytes());
                actual.0[2336..2368].copy_from_slice(&key.hash);
                actual.0[2368..].copy_from_slice(&key.rejection.0);
                assert!(ct::equal(&actual.0, &expected));
                count += 1;
            }
        }
        std::println!("NIST ML-KEM-768 expanded private keygen: {count} cases");
    }
}
