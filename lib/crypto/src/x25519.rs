//! RFC 7748 X25519. Secret multiplication uses the 255-step Montgomery ladder
//! with masked swaps and fixed-schedule inversion. Public inputs accept the
//! RFC's noncanonical field encodings. All-zero shared secrets are refused.
//! The caller supplies entropy; this module never queries the CPU for it.

use crate::{Error, Random, ct, field25519::F, secret::Secret};
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct PublicKey(pub [u8; 32]);
pub struct SecretKey(Secret<32>);
pub struct SharedSecret(Secret<32>);
impl SharedSecret {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0.0
    }
}
impl SecretKey {
    pub fn from_bytes(mut bytes: [u8; 32]) -> Self {
        bytes[0] &= 248;
        bytes[31] = (bytes[31] & 127) | 64;
        Self(Secret(bytes))
    }
    pub fn generate(random: &mut impl Random) -> Result<Self, Error> {
        let mut b = Secret([0; 32]);
        random.fill(&mut b.0)?;
        Ok(Self::from_bytes(b.0))
    }
    pub fn public_key(&self) -> PublicKey {
        let mut base = [0; 32];
        base[0] = 9;
        PublicKey(multiply(&self.0.0, &base).0)
    }
    pub fn agree(self, peer: &PublicKey) -> Result<SharedSecret, Error> {
        let result = multiply(&self.0.0, &peer.0);
        if ct::equal(&result.0, &[0; 32]) {
            Err(Error::InvalidPoint)
        } else {
            Ok(SharedSecret(result))
        }
    }
}
fn multiply(scalar: &[u8; 32], point: &[u8; 32]) -> Secret<32> {
    let x1 = F::from_bytes(point);
    let mut x2 = F::one();
    let mut z2 = F::zero();
    let mut x3 = F::from_bytes(point);
    let mut z3 = F::one();
    let mut swap = 0;
    for i in (0..255).rev() {
        let bit = u64::from((scalar[i / 8] >> (i % 8)) & 1);
        swap ^= bit;
        let xx = F::select(&x2, &x3, swap);
        x3 = F::select(&x3, &x2, swap);
        x2 = xx;
        let zz = F::select(&z2, &z3, swap);
        z3 = F::select(&z3, &z2, swap);
        z2 = zz;
        swap = bit;
        let a = x2.add(&z2);
        let aa = a.square();
        let b = x2.sub(&z2);
        let bb = b.square();
        let e = aa.sub(&bb);
        let c = x3.add(&z3);
        let d = x3.sub(&z3);
        let da = d.mul(&a);
        let cb = c.mul(&b);
        x3 = da.add(&cb).square();
        z3 = x1.mul(&da.sub(&cb).square());
        x2 = aa.mul(&bb);
        z2 = e.mul(&aa.add(&F::small(121665).mul(&e)));
    }
    x2 = F::select(&x2, &x3, swap);
    z2 = F::select(&z2, &z3, swap);
    Secret(x2.mul(&z2.invert()).bytes())
}
