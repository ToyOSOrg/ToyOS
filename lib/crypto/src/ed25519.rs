//! RFC 8032 pure Ed25519. Verification uses the cofactorless equation and
//! rejects noncanonical encodings, negative zero, noncanonical S and small-
//! order R or public keys. Lengths and refusal branches depend on public data.
//! Signing (feature `sign`) uses fixed-schedule field/scalar arithmetic and
//! complete Edwards addition; neither tables nor branches depend on secrets.

use crate::{Error, ct, field25519::F, hash::Sha512, scalar25519::S};
#[cfg(feature = "sign")]
use crate::{Random, secret::Secret};
const D: [u8; 32] = [
    0xa3, 0x78, 0x59, 0x13, 0xca, 0x4d, 0xeb, 0x75, 0xab, 0xd8, 0x41, 0x41, 0x4d, 0x0a, 0x70, 0x00,
    0x98, 0xe8, 0x79, 0x77, 0x79, 0x40, 0xc7, 0x8c, 0x73, 0xfe, 0x6f, 0x2b, 0xee, 0x6c, 0x03, 0x52,
];
const SQRT_M1: [u8; 32] = [
    0xb0, 0xa0, 0x0e, 0x4a, 0x27, 0x1b, 0xee, 0xc4, 0x78, 0xe4, 0x2f, 0xad, 0x06, 0x18, 0x43, 0x2f,
    0xa7, 0xd7, 0xfb, 0x3d, 0x99, 0x00, 0x4d, 0x2b, 0x0b, 0xdf, 0xc1, 0x4f, 0x80, 0x24, 0x83, 0x2b,
];
struct Point {
    x: F,
    y: F,
    z: F,
    t: F,
}
impl Point {
    fn identity() -> Self {
        Self {
            x: F::zero(),
            y: F::one(),
            z: F::one(),
            t: F::zero(),
        }
    }
    fn base() -> Self {
        let mut b = [0x66; 32];
        b[0] = 0x58;
        Self::decode(&b).expect("Ed25519 base point")
    }
    fn decode(input: &[u8; 32]) -> Result<Self, Error> {
        let mut b = *input;
        let sign = u64::from(b[31] >> 7);
        b[31] &= 127;
        let y = F::from_bytes(&b);
        if !ct::equal(&b, &y.bytes()) {
            return Err(Error::InvalidPoint);
        }
        let y2 = y.square();
        let u = y2.sub(&F::one());
        let v = F::from_bytes(&D).mul(&y2).add(&F::one());
        let q = u.mul(&v.invert());
        let mut exponent = [0xff; 32];
        exponent[0] = 0xfe;
        exponent[31] = 0x0f;
        let root = q.pow(&exponent, 252);
        let alt = root.mul(&F::from_bytes(&SQRT_M1));
        let root = F::select(&alt, &root, root.square().eq(&q));
        if root.square().eq(&q) == 0 || (root.eq(&F::zero()) == 1 && sign == 1) {
            return Err(Error::InvalidPoint);
        }
        let x = F::select(&root, &root.neg(), u64::from(root.bytes()[0] & 1) ^ sign);
        let t = x.mul(&y);
        Ok(Self {
            x,
            y,
            z: F::one(),
            t,
        })
    }
    #[cfg(feature = "sign")]
    fn encode(&self) -> [u8; 32] {
        let inverse = self.z.invert();
        let x = self.x.mul(&inverse);
        let mut y = self.y.mul(&inverse).bytes();
        y[31] |= (x.bytes()[0] & 1) << 7;
        y
    }
    fn add(&self, q: &Self) -> Self {
        let a = self.y.sub(&self.x).mul(&q.y.sub(&q.x));
        let b = self.y.add(&self.x).mul(&q.y.add(&q.x));
        let c = self.t.mul(&q.t).mul(&F::from_bytes(&D)).mul(&F::small(2));
        let d = self.z.mul(&q.z).mul(&F::small(2));
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
    fn select(a: &Self, b: &Self, bit: u64) -> Self {
        Self {
            x: F::select(&a.x, &b.x, bit),
            y: F::select(&a.y, &b.y, bit),
            z: F::select(&a.z, &b.z, bit),
            t: F::select(&a.t, &b.t, bit),
        }
    }
    fn mul(&self, scalar: &[u8; 32]) -> Self {
        let mut r = Self::identity();
        for i in (0..256).rev() {
            let double = r.add(&r);
            let added = double.add(self);
            r = Self::select(&double, &added, u64::from((scalar[i / 8] >> (i % 8)) & 1));
        }
        r
    }
    fn eq(&self, q: &Self) -> u64 {
        self.x.mul(&q.z).eq(&q.x.mul(&self.z)) & self.y.mul(&q.z).eq(&q.y.mul(&self.z))
    }
    fn small_order(&self) -> bool {
        let p = self.add(self);
        let p = p.add(&p);
        let p = p.add(&p);
        p.eq(&Self::identity()) == 1
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
