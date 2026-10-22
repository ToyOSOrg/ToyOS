//! RFC 8017 RSA signature verification only, 2048..=4096 bits. No private-key
//! operation or unbounded integer type exists. All RSA data is public; parsing,
//! arithmetic and refusal branches are permitted to be variable time. PSS
//! requires MGF1 to use the message hash and a salt exactly one digest long,
//! as required by TLS. DigestInfo for v1.5 must contain the specified NULL.
use crate::{
    Error, ct,
    hash::{Sha256, Sha384, Sha512},
    montgomery::{mac, n0},
};
#[derive(Clone, Copy, Debug)]
pub enum Hash {
    Sha256,
    Sha384,
    Sha512,
}
impl Hash {
    fn len(self) -> usize {
        match self {
            Self::Sha256 => 32,
            Self::Sha384 => 48,
            Self::Sha512 => 64,
        }
    }
    fn digest(self, parts: &[&[u8]]) -> Result<[u8; 64], Error> {
        let mut out = [0; 64];
        match self {
            Self::Sha256 => {
                let mut h = Sha256::new();
                for p in parts {
                    h.update(p)?
                }
                out[..32].copy_from_slice(&h.finalize())
            }
            Self::Sha384 => {
                let mut h = Sha384::new();
                for p in parts {
                    h.update(p)?
                }
                out[..48].copy_from_slice(&h.finalize())
            }
            Self::Sha512 => {
                let mut h = Sha512::new();
                for p in parts {
                    h.update(p)?
                }
                out.copy_from_slice(&h.finalize())
            }
        }
        Ok(out)
    }
}
pub struct PublicKey {
    modulus: [u64; WORDS],
    words: usize,
    bits: usize,
    exponent: u32,
    /// -modulus^-1 mod 2^64.
    n0: u64,
    /// R^2 mod modulus, R = 2^(64 * words).
    r2: [u64; WORDS],
}
/// The limbs of the largest modulus, 4096 bits.
const WORDS: usize = 64;
impl PublicKey {
    /// Minimal unsigned big-endian integers. ASN.1 belongs to the consumer.
    pub fn from_components(modulus: &[u8], exponent: &[u8]) -> Result<Self, Error> {
        if !(256..=512).contains(&modulus.len())
            || modulus[0] == 0
            || exponent.is_empty()
            || exponent.len() > 4
            || exponent[0] == 0
        {
            return Err(Error::InvalidKey);
        }
        let bits = modulus.len() * 8 - modulus[0].leading_zeros() as usize;
        let e = exponent.iter().fold(0u32, |e, &b| (e << 8) | u32::from(b));
        if bits < 2048 || e < 3 || e & 1 == 0 || modulus[modulus.len() - 1] & 1 == 0 {
            return Err(Error::InvalidKey);
        }
        // Whole groups of four words, which the instruction path takes; the
        // zero words above the modulus only enlarge R.
        let words = modulus.len().div_ceil(32) * 4;
        let m = read(modulus);
        let mut key = Self {
            modulus: m,
            words,
            bits,
            exponent: e,
            n0: n0(m[0]),
            r2: [0; WORDS],
        };
        key.r2 = key.montgomery_r2();
        Ok(key)
    }
    /// R^2 mod n from 2^(bits-1) < n: doublings to 2^words * R, then six
    /// Montgomery squarings, each taking 2^t * R to 2^(2t) * R, up to
    /// 2^(64 * words) * R.
    fn montgomery_r2(&self) -> [u64; WORDS] {
        let mut x = [0; WORDS];
        x[(self.bits - 1) / 64] = 1 << ((self.bits - 1) % 64);
        for _ in 0..64 * self.words - (self.bits - 1) + self.words {
            self.double(&mut x);
        }
        let mut y = [0; WORDS];
        for _ in 0..3 {
            self.mul(&x, &x, &mut y);
            self.mul(&y, &y, &mut x);
        }
        x
    }
    /// x = 2x mod n, for x < n.
    fn double(&self, x: &mut [u64; WORDS]) {
        let mut carry = 0;
        for word in &mut x[..self.words] {
            let top = *word >> 63;
            *word = (*word << 1) | carry;
            carry = top;
        }
        if carry != 0 || !less(x, &self.modulus, self.words) {
            subtract(x, &self.modulus, self.words);
        }
    }
    /// out = a * b / R mod n, for a, b < n, by the instruction path where
    /// the CPU has one.
    fn mul(&self, a: &[u64; WORDS], b: &[u64; WORDS], out: &mut [u64; WORDS]) {
        let n = self.words;
        match crate::arch::rsa_mul(a, b, &self.modulus, self.n0, n, out) {
            Some(top) => {
                if top != 0 || !less(out, &self.modulus, n) {
                    subtract(out, &self.modulus, n);
                }
            }
            None => self.mul_portable(a, b, out),
        }
    }
    /// Word-serial Montgomery multiplication with the reduction step
    /// interleaved (CIOS; Koc, Acar and Kaliski, "Analyzing and comparing
    /// Montgomery multiplication algorithms", IEEE Micro 1996).
    fn mul_portable(&self, a: &[u64; WORDS], b: &[u64; WORDS], out: &mut [u64; WORDS]) {
        let n = self.words;
        let (a, b, m) = (&a[..n], &b[..n], &self.modulus[..n]);
        let mut t = [0u64; WORDS + 2];
        let t = &mut t[..n + 2];
        for &bi in b {
            let mut c = 0;
            for j in 0..n {
                (t[j], c) = mac(t[j], a[j], bi, c);
            }
            let (s, overflow) = t[n].overflowing_add(c);
            t[n] = s;
            t[n + 1] = u64::from(overflow);
            let q = t[0].wrapping_mul(self.n0);
            let (_, mut c) = mac(t[0], q, m[0], 0);
            for j in 1..n {
                (t[j - 1], c) = mac(t[j], q, m[j], c);
            }
            let (s, overflow) = t[n].overflowing_add(c);
            t[n - 1] = s;
            t[n] = t[n + 1].wrapping_add(u64::from(overflow));
        }
        out[..n].copy_from_slice(&t[..n]);
        if t[n] != 0 || !less(out, &self.modulus, n) {
            subtract(out, &self.modulus, n);
        }
    }
    /// The signature raised to e, big-endian in the low bytes of 512.
    fn representative(&self, signature: &[u8]) -> Result<[u8; 512], Error> {
        if signature.len() != self.bits.div_ceil(8) {
            return Err(Error::Length);
        }
        let s = read(signature);
        if !less(&s, &self.modulus, self.words) {
            return Err(Error::InvalidSignature);
        }
        let value = self.power(&s);
        let mut bytes = [0; 512];
        for i in 0..WORDS {
            bytes[512 - 8 * (i + 1)..512 - 8 * i].copy_from_slice(&value[i].to_be_bytes())
        }
        Ok(bytes)
    }
    /// s^e mod n for s < n: s^(e-1) in Montgomery form by left-to-right
    /// square-and-multiply, then one Montgomery multiplication by the
    /// unencoded s, which also leaves Montgomery form. 65537 is 18
    /// multiplications.
    fn power(&self, s: &[u64; WORDS]) -> [u64; WORDS] {
        let mut base = [0; WORDS];
        self.mul(s, &self.r2, &mut base);
        // e - 1 is even and at least 2, so its top bit is the starting `base`.
        let e = self.exponent - 1;
        let mut x = base;
        let mut y = [0; WORDS];
        for i in (0..31 - e.leading_zeros()).rev() {
            self.mul(&x, &x, &mut y);
            if (e >> i) & 1 == 1 {
                self.mul(&y, &base, &mut x);
            } else {
                core::mem::swap(&mut x, &mut y);
            }
        }
        self.mul(&x, s, &mut y);
        y
    }
    pub fn verify_pkcs1_v1_5(
        &self,
        hash: Hash,
        message: &[u8],
        signature: &[u8],
    ) -> Result<(), Error> {
        let encoded = self.representative(signature)?;
        let encoded = &encoded[512 - self.bits.div_ceil(8)..];
        let h = hash.digest(&[message])?;
        let n = hash.len();
        let mut prefix = [
            0x30, 0, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0,
            0x05, 0, 0x04, 0,
        ];
        prefix[1] = (17 + n) as u8;
        prefix[14] = match hash {
            Hash::Sha256 => 1,
            Hash::Sha384 => 2,
            Hash::Sha512 => 3,
        };
        prefix[18] = n as u8;
        let padding = encoded.len() - prefix.len() - n - 3;
        if padding < 8
            || encoded[..2] != [0, 1]
            || encoded[2..2 + padding].iter().any(|&x| x != 255)
            || encoded[2 + padding] != 0
            || encoded[3 + padding..3 + padding + 19] != prefix
            || !ct::equal(&encoded[3 + padding + 19..], &h[..n])
        {
            return Err(Error::InvalidSignature);
        }
        Ok(())
    }
    pub fn verify_pss(&self, hash: Hash, message: &[u8], signature: &[u8]) -> Result<(), Error> {
        let encoded = self.representative(signature)?;
        let bits = self.bits - 1;
        let length = bits.div_ceil(8);
        let encoded_start = 512 - self.bits.div_ceil(8);
        let start = 512 - length;
        if encoded[encoded_start..start].iter().any(|&b| b != 0) {
            return Err(Error::InvalidSignature);
        }
        let encoded = &encoded[start..];
        let n = hash.len();
        if length < 2 * n + 2 || encoded[length - 1] != 0xbc {
            return Err(Error::InvalidSignature);
        }
        let db_len = length - n - 1;
        let excess = 8 * length - bits;
        if encoded[0] & ((0xffu16 << (8 - excess)) as u8) != 0 {
            return Err(Error::InvalidSignature);
        }
        let h = &encoded[db_len..db_len + n];
        let mut db = [0; 512];
        db[..db_len].copy_from_slice(&encoded[..db_len]);
        for (i, chunk) in db[..db_len].chunks_mut(n).enumerate() {
            let mask = hash.digest(&[h, &(i as u32).to_be_bytes()])?;
            for (j, byte) in chunk.iter_mut().enumerate() {
                *byte ^= mask[j]
            }
        }
        db[0] &= 0xff >> excess;
        let padding = db_len - n - 1;
        if db[..padding].iter().any(|&b| b != 0) || db[padding] != 1 {
            return Err(Error::InvalidSignature);
        }
        let message_hash = hash.digest(&[message])?;
        let expected = hash.digest(&[&[0; 8], &message_hash[..n], &db[padding + 1..db_len]])?;
        if !ct::equal(h, &expected[..n]) {
            return Err(Error::InvalidSignature);
        }
        Ok(())
    }
}
/// a < b over the low `n` words.
fn less(a: &[u64; WORDS], b: &[u64; WORDS], n: usize) -> bool {
    for i in (0..n).rev() {
        if a[i] != b[i] {
            return a[i] < b[i];
        }
    }
    false
}
/// a -= b over the low `n` words, modulo 2^(64n).
fn subtract(a: &mut [u64; WORDS], b: &[u64; WORDS], n: usize) {
    let mut borrow = false;
    for i in 0..n {
        let (d, b1) = a[i].overflowing_sub(b[i]);
        let (d, b2) = d.overflowing_sub(u64::from(borrow));
        a[i] = d;
        borrow = b1 | b2;
    }
}
fn read(bytes: &[u8]) -> [u64; WORDS] {
    let mut out = [0; WORDS];
    for (i, &byte) in bytes.iter().rev().enumerate() {
        out[i / 8] |= u64::from(byte) << (8 * (i % 8))
    }
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    use num_bigint::BigUint;

    /// Odd moduli of every limb shape, including partial top limbs, and
    /// exponents up to four bytes against the integer oracle.
    #[test]
    fn power_and_r2_match_integer_oracle() {
        let mut seed = 0x9e3779b97f4a7c15u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed as u8
        };
        for len in [256, 257, 263, 300, 384, 511, 512] {
            let mut modulus: std::vec::Vec<u8> = (0..len).map(|_| next()).collect();
            modulus[0] |= 0x80 >> (len % 8);
            modulus[len - 1] |= 1;
            let n = BigUint::from_bytes_be(&modulus);
            for e in [3u32, 17, 65537, 0x8000_0001, 0xffff_ffff] {
                let key = PublicKey::from_components(&modulus, &e.to_be_bytes()[e.leading_zeros() as usize / 8..])
                    .unwrap();
                let r = BigUint::from(1u8) << (64 * key.words);
                assert_eq!(integer(&key.r2), (&r * &r) % &n);
                for _ in 0..4 {
                    let s: std::vec::Vec<u8> = (0..len).map(|_| next()).collect();
                    let s = BigUint::from_bytes_be(&s) % &n;
                    let mut bytes = std::vec![0; len];
                    let raw = s.to_bytes_be();
                    bytes[len - raw.len()..].copy_from_slice(&raw);
                    let ours = key.representative(&bytes).unwrap();
                    assert_eq!(BigUint::from_bytes_be(&ours), s.modpow(&BigUint::from(e), &n));
                }
                let mut n_bytes = modulus.clone();
                assert_eq!(key.representative(&n_bytes), Err(Error::InvalidSignature));
                n_bytes[len - 1] -= 1;
                assert!(key.representative(&n_bytes).is_ok());
            }
        }
    }

    /// The instruction path, where the CPU has one, equals the portable
    /// multiplication for the padded word counts 32 to 64 in steps of 8, and
    /// at the largest residues.
    #[test]
    fn instruction_path_matches_the_portable_multiplication() {
        // Which path this run compares, for a reader of `--nocapture` output.
        let (zero, modulus) = ([0u64; WORDS], [u64::MAX; WORDS]);
        let mut out = [0u64; WORDS];
        let kernel = crate::arch::rsa_mul(&zero, &zero, &modulus, 1, 4, &mut out).is_some();
        std::println!("instruction path: {}", if kernel { "the BMI2/ADX kernel" } else { "none; portable against itself" });
        let mut seed = 0x2545f4914f6cdd1du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for len in [256usize, 300, 384, 448, 512] {
            let mut modulus: std::vec::Vec<u8> = (0..len).map(|_| next() as u8).collect();
            modulus[0] |= 0x80;
            modulus[len - 1] |= 1;
            let key = PublicKey::from_components(&modulus, &[1, 0, 1]).unwrap();
            let n = BigUint::from_bytes_be(&modulus);
            for case in 0..64 {
                let mut a = [0u64; WORDS];
                let mut b = [0u64; WORDS];
                let (x, y) = if case == 0 {
                    (&n - 1u8, &n - 1u8)
                } else {
                    let x: std::vec::Vec<u8> = (0..len).map(|_| next() as u8).collect();
                    let y: std::vec::Vec<u8> = (0..len).map(|_| next() as u8).collect();
                    (BigUint::from_bytes_be(&x) % &n, BigUint::from_bytes_be(&y) % &n)
                };
                for (i, d) in x.to_u64_digits().iter().enumerate() {
                    a[i] = *d;
                }
                for (i, d) in y.to_u64_digits().iter().enumerate() {
                    b[i] = *d;
                }
                let (mut ours, mut portable) = ([0; WORDS], [0; WORDS]);
                key.mul(&a, &b, &mut ours);
                key.mul_portable(&a, &b, &mut portable);
                assert_eq!(ours, portable, "{len} bytes, case {case}");
            }
        }
    }

    fn integer(words: &[u64; WORDS]) -> BigUint {
        BigUint::from_bytes_le(&words.iter().flat_map(|w| w.to_le_bytes()).collect::<std::vec::Vec<_>>())
    }
}
