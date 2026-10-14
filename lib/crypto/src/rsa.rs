//! RFC 8017 RSA signature verification only, 2048..=4096 bits. No private-key
//! operation or unbounded integer type exists. All RSA data is public; parsing,
//! arithmetic and refusal branches are permitted to be variable time. PSS
//! requires MGF1 to use the message hash and a salt exactly one digest long,
//! as required by TLS. DigestInfo for v1.5 must contain the specified NULL.
use crate::{
    Error, ct,
    hash::{Sha256, Sha384, Sha512},
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
    modulus: [u64; 64],
    words: usize,
    bits: usize,
    exponent: u32,
    inverse: u64,
    r2: [u64; 64],
    one: [u64; 64],
}
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
        let words = modulus.len().div_ceil(8);
        let m = read(modulus);
        let mut inverse = 1u64;
        for _ in 0..6 {
            inverse = inverse.wrapping_mul(2u64.wrapping_sub(m[0].wrapping_mul(inverse)))
        }
        let mut key = Self {
            modulus: m,
            words,
            bits,
            exponent: e,
            inverse: inverse.wrapping_neg(),
            r2: [0; 64],
            one: [0; 64],
        };
        let mut value = [0; 64];
        value[0] = 1;
        for _ in 0..2 * words * 64 {
            let mut d = [0; 64];
            let mut c = 0;
            for i in 0..words {
                let t = u128::from(value[i]) * 2 + c;
                d[i] = t as u64;
                c = t >> 64
            }
            value = key.reduce(d, c as u64)
        }
        key.r2 = value;
        let mut raw = [0; 64];
        raw[0] = 1;
        key.one = key.mul(&raw, &key.r2);
        Ok(key)
    }
    fn reduce(&self, value: [u64; 64], high: u64) -> [u64; 64] {
        let mut d = [0; 64];
        let mut borrow = 0u128;
        for i in 0..self.words {
            let t = (1u128 << 64) + u128::from(value[i]) - u128::from(self.modulus[i]) - borrow;
            d[i] = t as u64;
            borrow = 1 - (t >> 64)
        }
        if high != 0 || borrow == 0 { d } else { value }
    }
    fn mul(&self, a: &[u64; 64], b: &[u64; 64]) -> [u64; 64] {
        let n = self.words;
        let mut t = [0u64; 130];
        for i in 0..n {
            let mut c = 0u128;
            for j in 0..n {
                let v = u128::from(a[i]) * u128::from(b[j]) + u128::from(t[i + j]) + c;
                t[i + j] = v as u64;
                c = v >> 64
            }
            t[i + n] = c as u64
        }
        for i in 0..n {
            let q = t[i].wrapping_mul(self.inverse);
            let mut c = 0u128;
            for j in 0..n {
                let v = u128::from(q) * u128::from(self.modulus[j]) + u128::from(t[i + j]) + c;
                t[i + j] = v as u64;
                c = v >> 64
            }
            for word in t.iter_mut().take(2 * n + 1).skip(i + n) {
                let v = u128::from(*word) + c;
                *word = v as u64;
                c = v >> 64
            }
            assert_eq!(c, 0)
        }
        let mut result = [0; 64];
        result[..n].copy_from_slice(&t[n..2 * n]);
        self.reduce(result, t[2 * n])
    }
    fn representative(&self, signature: &[u8]) -> Result<[u8; 512], Error> {
        if signature.len() != self.bits.div_ceil(8) {
            return Err(Error::Length);
        }
        let raw = read(signature);
        let reduced = self.reduce(raw, 0);
        if raw != reduced {
            return Err(Error::InvalidSignature);
        }
        let base = self.mul(&raw, &self.r2);
        let mut r = self.one;
        for i in (0..32).rev() {
            r = self.mul(&r, &r);
            if (self.exponent >> i) & 1 == 1 {
                r = self.mul(&r, &base)
            }
        }
        let mut one = [0; 64];
        one[0] = 1;
        let value = self.mul(&r, &one);
        let mut bytes = [0; 512];
        for i in 0..64 {
            bytes[512 - 8 * (i + 1)..512 - 8 * i].copy_from_slice(&value[i].to_be_bytes())
        }
        Ok(bytes)
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
fn read(bytes: &[u8]) -> [u64; 64] {
    let mut out = [0; 64];
    for (i, &byte) in bytes.iter().rev().enumerate() {
        out[i / 8] |= u64::from(byte) << (8 * (i % 8))
    }
    out
}
