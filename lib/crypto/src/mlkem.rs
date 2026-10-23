//! FIPS 203 ML-KEM-768, for TLS and SSH hybrid exchange. Secret polynomial
//! operations, CBD sampling, hashes and re-encryption have fixed schedules,
//! and no secret value is divided. Matrix rejection sampling depends only on the
//! public seed. Public-key decoding rejects coefficients >=3329. Decapsulation
//! implements masked implicit rejection, with no validity bit or error oracle.
//! Secret keys can only originate from the caller's entropy source, so external
//! expanded decapsulation-key validation is outside this API.

use crate::{
    Error, Random, ct,
    keccak::{self, Sponge},
    secret::{Secret, wipe},
};
const Q: u32 = 3329;
struct Poly([u16; 256]);
impl Drop for Poly {
    fn drop(&mut self) {
        for word in &mut self.0 {
            // SAFETY: the address is a live, exclusively borrowed coefficient.
            unsafe { core::ptr::write_volatile(word, 0) }
        }
        core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst)
    }
}
/// ⌊n / q⌋ for n < 2^33 / 623, which holds every value reduced or compressed
/// here, by a multiply and shift: a divide's latency can depend on its
/// operands (KyberSlash, Bernstein et al., IACR ePrint 2024/1049). With
/// M = ⌈2^33 / q⌉, n·M / 2^33 = n / q + 623·n / (q·2^33), and the excess is
/// below 1/q, too small to carry the quotient past the next integer.
fn divide(n: u32) -> u32 {
    const M: u64 = (1u64 << 33).div_ceil(Q as u64);
    ((u64::from(n) * M) >> 33) as u32
}
fn reduce(n: u32) -> u16 {
    (n - divide(n) * Q) as u16
}
fn add(a: u16, b: u16) -> u16 {
    reduce(u32::from(a) + u32::from(b))
}
fn sub(a: u16, b: u16) -> u16 {
    reduce(u32::from(a) + Q - u32::from(b))
}
fn mul(a: u16, b: u16) -> u16 {
    reduce(u32::from(a) * u32::from(b))
}
/// ζ^BitRev7(i) and, for `odd`, γ_i = ζ^(2·BitRev7(i)+1), with ζ = 17: FIPS 203
/// Appendix A's two tables.
const fn powers(odd: bool) -> [u16; 128] {
    let mut out = [0; 128];
    let mut i = 0;
    while i < 128 {
        let reversed = ((i as u8).reverse_bits() >> 1) as u32;
        let exponent = if odd { 2 * reversed + 1 } else { reversed };
        let mut power = 1;
        let mut k = 0;
        while k < exponent {
            power = power * 17 % Q;
            k += 1
        }
        out[i] = power as u16;
        i += 1
    }
    out
}
const ZETAS: [u16; 128] = powers(false);
const GAMMAS: [u16; 128] = powers(true);
impl Poly {
    fn zero() -> Self {
        Self([0; 256])
    }
    fn ntt(mut self) -> Self {
        let mut k = 1;
        let mut len = 128;
        while len >= 2 {
            for start in (0..256).step_by(2 * len) {
                let z = ZETAS[k];
                k += 1;
                for i in start..start + len {
                    let t = mul(z, self.0[i + len]);
                    self.0[i + len] = sub(self.0[i], t);
                    self.0[i] = add(self.0[i], t)
                }
            }
            len /= 2
        }
        self
    }
    fn inverse(mut self) -> Self {
        let mut k = 127;
        let mut len = 2;
        while len <= 128 {
            for start in (0..256).step_by(2 * len) {
                let z = ZETAS[k];
                k -= 1;
                for i in start..start + len {
                    let t = self.0[i];
                    self.0[i] = add(t, self.0[i + len]);
                    self.0[i + len] = mul(z, sub(self.0[i + len], t))
                }
            }
            len *= 2
        }
        for word in &mut self.0 {
            *word = mul(*word, 3303)
        }
        self
    }
    fn product(&self, q: &Self) -> Self {
        let mut out = Self::zero();
        for i in 0..128 {
            let gamma = GAMMAS[i];
            out.0[2 * i] = add(
                mul(self.0[2 * i], q.0[2 * i]),
                mul(gamma, mul(self.0[2 * i + 1], q.0[2 * i + 1])),
            );
            out.0[2 * i + 1] = add(
                mul(self.0[2 * i], q.0[2 * i + 1]),
                mul(self.0[2 * i + 1], q.0[2 * i]),
            );
        }
        out
    }
    fn plus(&self, b: &Self) -> Self {
        Self(core::array::from_fn(|i| add(self.0[i], b.0[i])))
    }
    fn minus(&self, b: &Self) -> Self {
        Self(core::array::from_fn(|i| sub(self.0[i], b.0[i])))
    }
    fn encode(&self, d: usize, output: &mut [u8]) {
        assert_eq!(output.len(), 32 * d);
        output.fill(0);
        for i in 0..256 {
            for bit in 0..d {
                let at = i * d + bit;
                output[at / 8] |= (((self.0[i] >> bit) & 1) as u8) << (at % 8)
            }
        }
    }
    fn decode(d: usize, input: &[u8]) -> Self {
        assert_eq!(input.len(), 32 * d);
        let mut out = Self::zero();
        for i in 0..256 {
            for bit in 0..d {
                let at = i * d + bit;
                out.0[i] |= u16::from((input[at / 8] >> (at % 8)) & 1) << bit
            }
        }
        out
    }
    fn compress(mut self, d: usize) -> Self {
        for word in &mut self.0 {
            *word = (divide((u32::from(*word) << d) + Q / 2) & ((1 << d) - 1)) as u16
        }
        self
    }
    fn decompress(mut self, d: usize) -> Self {
        for word in &mut self.0 {
            *word = ((Q * u32::from(*word) + (1 << (d - 1))) >> d) as u16
        }
        self
    }
}
fn noise(seed: &[u8; 32], nonce: u8) -> Poly {
    let mut bytes = Secret([0; 128]);
    Sponge::new(136, 31, &[seed, &[nonce]]).read(&mut bytes.0);
    let mut out = Poly::zero();
    for i in 0..256 {
        let bits = (bytes.0[i / 2] >> (4 * (i % 2))) & 15;
        let a = u32::from(bits & 1) + u32::from((bits >> 1) & 1);
        let b = u32::from((bits >> 2) & 1) + u32::from((bits >> 3) & 1);
        out.0[i] = reduce(Q + a - b)
    }
    out
}
fn sample(rho: &[u8; 32], row: usize, col: usize) -> Poly {
    let mut xof = Sponge::new(168, 31, &[rho, &[col as u8, row as u8]]);
    let mut out = Poly::zero();
    let mut pos = 0;
    while pos < 256 {
        let mut b = [0; 3];
        xof.read(&mut b);
        for v in [
            u16::from(b[0]) | ((u16::from(b[1]) & 15) << 8),
            (u16::from(b[1]) >> 4) | (u16::from(b[2]) << 4),
        ] {
            if u32::from(v) < Q && pos < 256 {
                out.0[pos] = v;
                pos += 1
            }
        }
    }
    out
}
pub struct EncapsulationKey {
    bytes: [u8; 1184],
}
pub struct DecapsulationKey {
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
            let p = Poly::decode(12, chunk);
            if p.0.iter().any(|&x| u32::from(x) >= Q) {
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
            let mut sum = Poly::zero();
            for (j, yp) in y.iter().enumerate() {
                sum = sum.plus(&sample(rho, j, i).product(yp))
            }
            sum.inverse()
                .plus(&noise(coins, (i + 3) as u8))
                .compress(10)
                .encode(10, &mut out[i * 320..i * 320 + 320]);
        }
        let mut sum = Poly::zero();
        for (j, yp) in y.iter().enumerate() {
            sum = sum.plus(&Poly::decode(12, &self.bytes[j * 384..j * 384 + 384]).product(yp))
        }
        sum.inverse()
            .plus(&noise(coins, 6))
            .plus(&Poly::decode(1, message).decompress(1))
            .compress(4)
            .encode(4, &mut out[960..]);
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
            let mut sum = noise(sigma, (i + 3) as u8).ntt();
            for (j, s) in secret.iter().enumerate() {
                sum = sum.plus(&sample(rho, i, j).product(s))
            }
            sum.encode(12, &mut bytes[i * 384..i * 384 + 384])
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
        let mut sum = Poly::zero();
        for (i, s) in self.secret.iter().enumerate() {
            sum = sum.plus(
                &s.product(
                    &Poly::decode(10, &ciphertext[i * 320..i * 320 + 320])
                        .decompress(10)
                        .ntt(),
                ),
            )
        }
        let mut message = Secret([0; 32]);
        Poly::decode(4, &ciphertext[960..])
            .decompress(4)
            .minus(&sum.inverse())
            .compress(1)
            .encode(1, &mut message.0);
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
        let end = ((1u64 << 33) / 623) as u32;
        assert!(u64::from(end) > u64::from(Q) * u64::from(Q));
        for n in 0..end {
            assert_eq!(divide(n), n / Q, "{n}");
        }
    }

    /// Every pair of reduced coefficients, against 64-bit integers.
    #[test]
    #[cfg_attr(miri, ignore = "3329^2 pairs")]
    fn reduction_modulo_q_is_exact_over_its_whole_input_range() {
        for a in 0..Q as u16 {
            for b in 0..Q as u16 {
                let (x, y, q) = (u64::from(a), u64::from(b), u64::from(Q));
                assert_eq!(u64::from(add(a, b)), (x + y) % q, "{a} + {b}");
                assert_eq!(u64::from(sub(a, b)), (x + q - y) % q, "{a} - {b}");
                assert_eq!(u64::from(mul(a, b)), x * y % q, "{a} * {b}");
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
            for start in (0..Q as u16).step_by(256) {
                for (i, word) in x.0.iter_mut().enumerate() {
                    *word = (start + i as u16).min(Q as u16 - 1);
                }
                let compressed = Poly(x.0).compress(d);
                for (i, &value) in x.0.iter().enumerate() {
                    let want = ((u64::from(value) << (d + 1)) + u64::from(Q)) / (2 * u64::from(Q)) % (1 << d);
                    assert_eq!(u64::from(compressed.0[i]), want, "Compress_{d}({value})");
                }
            }
            let mut y = Poly::zero();
            for start in (0..1u32 << d).step_by(256) {
                for (i, word) in y.0.iter_mut().enumerate() {
                    *word = (start + i as u32).min((1 << d) - 1) as u16;
                }
                let decompressed = Poly(y.0).decompress(d);
                for (i, &value) in y.0.iter().enumerate() {
                    let want = (2 * u64::from(Q) * u64::from(value) + (1 << d)) >> (d + 1);
                    assert_eq!(u64::from(decompressed.0[i]), want, "Decompress_{d}({value})");
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
                    p.encode(12, &mut actual.0[384 * i..384 * i + 384])
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
