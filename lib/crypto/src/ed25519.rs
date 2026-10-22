//! RFC 8032 pure Ed25519. Verification uses the cofactorless equation and
//! rejects noncanonical encodings, negative zero, noncanonical S and small-
//! order R or public keys. Lengths and refusal branches depend on public data.
//! Signing (feature `sign`) uses fixed-schedule field/scalar arithmetic and
//! complete Edwards addition; neither tables nor branches depend on secrets.

use crate::{
    Error, ct,
    field25519::{Multiply, Tight},
    hash::Sha512,
    scalar25519::S,
};
#[cfg(feature = "sign")]
use crate::{Random, secret::Secret};
/// 2d for the curve's d = -121665/121666 (RFC 8032 section 5.1).
const D2: Tight = Tight::from_bytes(&[
    0x59, 0xf1, 0xb2, 0x26, 0x94, 0x9b, 0xd6, 0xeb, 0x56, 0xb1, 0x83, 0x82, 0x9a, 0x14, 0xe0, 0x00,
    0x30, 0xd1, 0xf3, 0xee, 0xf2, 0x80, 0x8e, 0x19, 0xe7, 0xfc, 0xdf, 0x56, 0xdc, 0xd9, 0x06, 0x24,
]);
const D: Tight = Tight::from_bytes(&[
    0xa3, 0x78, 0x59, 0x13, 0xca, 0x4d, 0xeb, 0x75, 0xab, 0xd8, 0x41, 0x41, 0x4d, 0x0a, 0x70, 0x00,
    0x98, 0xe8, 0x79, 0x77, 0x79, 0x40, 0xc7, 0x8c, 0x73, 0xfe, 0x6f, 0x2b, 0xee, 0x6c, 0x03, 0x52,
]);
const SQRT_M1: Tight = Tight::from_bytes(&[
    0xb0, 0xa0, 0x0e, 0x4a, 0x27, 0x1b, 0xee, 0xc4, 0x78, 0xe4, 0x2f, 0xad, 0x06, 0x18, 0x43, 0x2f,
    0xa7, 0xd7, 0xfb, 0x3d, 0x99, 0x00, 0x4d, 0x2b, 0x0b, 0xdf, 0xc1, 0x4f, 0x80, 0x24, 0x83, 0x2b,
]);
/// The base point B's encoding (RFC 8032 section 5.1): y = 4/5, x positive.
const B_ENCODED: [u8; 32] = [
    0x58, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66,
    0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66,
];
const B_X: Tight = Tight::from_bytes(&[
    0x1a, 0xd5, 0x25, 0x8f, 0x60, 0x2d, 0x56, 0xc9, 0xb2, 0xa7, 0x25, 0x95, 0x60, 0xc7, 0x2c, 0x69,
    0x5c, 0xdc, 0xd6, 0xfd, 0x31, 0xe2, 0xa4, 0xc0, 0xfe, 0x53, 0x6e, 0xcd, 0xd3, 0x36, 0x69, 0x21,
]);
const B_Y: Tight = Tight::from_bytes(&B_ENCODED);
/// x * y of B.
const B_T: Tight = Tight::from_bytes(&[
    0xa3, 0xdd, 0xb7, 0xa5, 0xb3, 0x8a, 0xde, 0x6d, 0xf5, 0x52, 0x51, 0x77, 0x80, 0x9f, 0xf0, 0x20,
    0x7d, 0xe3, 0xab, 0x64, 0x8e, 0x4e, 0xea, 0x66, 0x65, 0x76, 0x8b, 0xd7, 0x0f, 0x5f, 0x87, 0x67,
]);
/// Extended coordinates (X : Y : Z : T) with x = X/Z, y = Y/Z, xy = T/Z.
#[derive(Clone, Copy)]
struct Point {
    x: Tight,
    y: Tight,
    z: Tight,
    t: Tight,
}
impl Point {
    fn identity() -> Self {
        Self {
            x: Tight::ZERO,
            y: Tight::ONE,
            z: Tight::ONE,
            t: Tight::ZERO,
        }
    }
    fn base() -> Self {
        Self {
            x: B_X,
            y: B_Y,
            z: Tight::ONE,
            t: B_T,
        }
    }
    /// RFC 8032 section 5.1.3. Refusal branches depend on the public encoding.
    fn decode(input: &[u8; 32]) -> Result<Self, Error> {
        let mut b = *input;
        let sign = u64::from(b[31] >> 7);
        b[31] &= 127;
        let y = Tight::from_bytes(&b);
        if !ct::equal(&b, &y.bytes()) {
            return Err(Error::InvalidPoint);
        }
        let y2 = y.square();
        let u = y2.sub(&Tight::ONE);
        let v = D.mul(&y2).add(&Tight::ONE).carry();
        let q = u.mul(&v.invert());
        let root = q.mul(&q.pow_p58());
        let alt = root.mul(&SQRT_M1);
        let root = Tight::select(&alt, &root, root.square().eq(&q));
        if root.square().eq(&q) == 0 || (root.is_zero() == 1 && sign == 1) {
            return Err(Error::InvalidPoint);
        }
        let x = Tight::select(&root, &root.neg().carry(), root.is_negative() ^ sign);
        let t = x.mul(&y);
        Ok(Self {
            x,
            y,
            z: Tight::ONE,
            t,
        })
    }
    #[cfg(feature = "sign")]
    fn encode(&self) -> [u8; 32] {
        let inverse = self.z.invert();
        let x = self.x.mul(&inverse);
        let mut y = self.y.mul(&inverse).bytes();
        y[31] |= (x.is_negative() as u8) << 7;
        y
    }
    /// RFC 8032 section 5.1.4's addition, complete on the curve.
    fn add(&self, q: &Self) -> Self {
        let a = self.y.sub(&self.x).mul(&q.y.sub(&q.x));
        let b = self.y.add(&self.x).mul(&q.y.add(&q.x));
        let c = self.t.mul(&q.t).mul(&D2);
        let d = self.z.mul(&q.z.add(&q.z));
        let e = b.sub(&a);
        let f = d.sub(&c);
        let g = d.add(&c);
        let h = b.add(&a);
        Self {
            x: e.mul(&f),
            y: g.mul(&h),
            t: e.mul(&h),
            z: f.mul(&g),
        }
    }
    /// RFC 8032 section 5.1.4's doubling.
    fn double(&self) -> Self {
        let a = self.x.square();
        let b = self.y.square();
        let c = self.z.square().mul_small(2);
        let h = a.add(&b).carry();
        let e = h.sub(&self.x.add(&self.y).square());
        let g = a.sub(&b).carry();
        let f = c.add(&g);
        Self {
            x: e.mul(&f),
            y: g.mul(&h),
            t: e.mul(&h),
            z: f.mul(&g),
        }
    }
    fn select(a: &Self, b: &Self, bit: u64) -> Self {
        Self {
            x: Tight::select(&a.x, &b.x, bit),
            y: Tight::select(&a.y, &b.y, bit),
            z: Tight::select(&a.z, &b.z, bit),
            t: Tight::select(&a.t, &b.t, bit),
        }
    }
    /// scalar * self, on a fixed schedule.
    fn mul(&self, scalar: &[u8; 32]) -> Self {
        let mut r = Self::identity();
        for i in (0..256).rev() {
            let double = r.double();
            let added = double.add(self);
            r = Self::select(&double, &added, u64::from((scalar[i / 8] >> (i % 8)) & 1));
        }
        r
    }
    fn eq(&self, q: &Self) -> u64 {
        self.x.mul(&q.z).eq(&q.x.mul(&self.z)) & self.y.mul(&q.z).eq(&q.y.mul(&self.z))
    }
    fn small_order(&self) -> bool {
        self.double().double().double().eq(&Self::identity()) == 1
    }
}


pub struct PublicKey {
    encoded: [u8; 32],
    point: Point,
}
impl PublicKey {
    pub fn from_bytes(encoded: [u8; 32]) -> Result<Self, Error> {
        let point = Point::decode(&encoded)?;
        if point.small_order() {
            return Err(Error::InvalidKey);
        }
        Ok(Self { encoded, point })
    }
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.encoded
    }
    pub fn verify(&self, message: &[u8], signature: &[u8]) -> Result<(), Error> {
        if signature.len() != 64 {
            return Err(Error::Length);
        }
        let r_bytes: &[u8; 32] = signature[..32].try_into().unwrap();
        let s_bytes: &[u8; 32] = signature[32..].try_into().unwrap();
        let r = Point::decode(r_bytes)?;
        if r.small_order() {
            return Err(Error::InvalidSignature);
        }
        let s = S::canonical(s_bytes).ok_or(Error::InvalidSignature)?;
        let mut h = Sha512::new();
        h.update(r_bytes)?;
        h.update(&self.encoded)?;
        h.update(message)?;
        let digest = h.finalize();
        let k = S::reduce(&digest);
        if Point::base()
            .mul(&s.bytes())
            .eq(&r.add(&self.point.mul(&k.bytes())))
            == 1
        {
            Ok(())
        } else {
            Err(Error::InvalidSignature)
        }
    }
}
#[cfg(feature = "sign")]
pub struct SigningKey {
    expanded: Secret<64>,
    public: PublicKey,
}
#[cfg(feature = "sign")]
impl SigningKey {
    pub fn from_seed(seed: [u8; 32]) -> Self {
        let seed = Secret(seed);
        let mut expanded = Secret(Sha512::digest(&seed.0).expect("fixed 32-byte seed"));
        expanded.0[0] &= 248;
        expanded.0[31] = (expanded.0[31] & 63) | 64;
        let point = Point::base().mul(expanded.0[..32].try_into().unwrap());
        let public = PublicKey {
            encoded: point.encode(),
            point,
        };
        Self { expanded, public }
    }
    pub fn generate(random: &mut impl Random) -> Result<Self, Error> {
        let mut seed = Secret([0; 32]);
        random.fill(&mut seed.0)?;
        Ok(Self::from_seed(seed.0))
    }
    pub fn public_key(&self) -> &PublicKey {
        &self.public
    }
    pub fn sign(&self, message: &[u8]) -> Result<[u8; 64], Error> {
        let mut h = Sha512::new();
        h.update(&self.expanded.0[32..])?;
        h.update(message)?;
        let digest = Secret(h.finalize());
        let r = S::reduce(&digest.0);
        let r_bytes = Secret(r.bytes());
        let encoded = Point::base().mul(&r_bytes.0).encode();
        let mut h = Sha512::new();
        h.update(&encoded)?;
        h.update(self.public.as_bytes())?;
        h.update(message)?;
        let k = Secret(h.finalize());
        let k = S::reduce(&k.0);
        let a = S::reduce(&self.expanded.0[..32]);
        let s = r.add(&k.mul(&a));
        let mut out = [0; 64];
        out[..32].copy_from_slice(&encoded);
        out[32..].copy_from_slice(&s.bytes());
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The compile-time constants are what their definitions compute.
    #[test]
    fn constants_match_their_definitions() {
        let decoded = Point::decode(&B_ENCODED).unwrap();
        let base = Point::base();
        for (a, b) in [(&decoded.x, &base.x), (&decoded.y, &base.y), (&decoded.t, &base.t)] {
            assert_eq!(a.bytes(), b.bytes());
        }
        assert_eq!(D.add(&D).carry().bytes(), D2.bytes());
        let small = |x: u32| {
            let mut b = [0; 32];
            b[..4].copy_from_slice(&x.to_le_bytes());
            Tight::from_bytes(&b)
        };
        let d = small(121665).neg().mul(&small(121666).invert());
        assert_eq!(d.bytes(), D.bytes());
        assert_eq!(SQRT_M1.square().bytes(), Tight::ONE.neg().carry().bytes());
    }
}
