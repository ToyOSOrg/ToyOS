//! SEC 1 NIST P-256/P-384 arithmetic. ECDH uses fixed-width scalars and a
//! double-and-add schedule with complete masked handling of exceptional points.
//! SEC 1 decoding, DER decoding, ECDSA verification and key-validity refusals
//! depend on public data. Only uncompressed SEC 1 points are accepted.

use crate::{
    Error,
    montgomery::{Mod, V},
};
pub(crate) struct Curve<const N: usize> {
    pub(crate) p: Mod<N>,
    pub(crate) n: Mod<N>,
    b: V<N>,
    gx: V<N>,
    gy: V<N>,
}
pub(crate) struct Point<const N: usize> {
    x: V<N>,
    y: V<N>,
    z: V<N>,
}
impl<const N: usize> Point<N> {
    fn identity(c: &Curve<N>) -> Self {
        Self {
            x: V::zero(),
            y: c.p.one(),
            z: V::zero(),
        }
    }
    fn select(a: &Self, b: &Self, bit: u64) -> Self {
        Self {
            x: V::select(&a.x, &b.x, bit),
            y: V::select(&a.y, &b.y, bit),
            z: V::select(&a.z, &b.z, bit),
        }
    }
    fn double(&self, c: &Curve<N>) -> Self {
        let p = &c.p;
        let delta = p.square(&self.z);
        let gamma = p.square(&self.y);
        let beta = p.mul(&self.x, &gamma);
        let alpha = p.mul(
            &p.small(3),
            &p.mul(&p.sub(&self.x, &delta), &p.add(&self.x, &delta)),
        );
        let x = p.sub(&p.square(&alpha), &p.mul(&p.small(8), &beta));
        let z = p.sub(&p.sub(&p.square(&p.add(&self.y, &self.z)), &gamma), &delta);
        let y = p.sub(
            &p.mul(&alpha, &p.sub(&p.mul(&p.small(4), &beta), &x)),
            &p.mul(&p.small(8), &p.square(&gamma)),
        );
        let out = Self { x, y, z };
        Self::select(&out, &Self::identity(c), self.z.eq(&V::zero()))
    }
    fn add(&self, q: &Self, c: &Curve<N>) -> Self {
        let p = &c.p;
        let z1 = p.square(&self.z);
        let z2 = p.square(&q.z);
        let u1 = p.mul(&self.x, &z2);
        let u2 = p.mul(&q.x, &z1);
        let s1 = p.mul(&self.y, &p.mul(&q.z, &z2));
        let s2 = p.mul(&q.y, &p.mul(&self.z, &z1));
        let h = p.sub(&u2, &u1);
        let r = p.sub(&s2, &s1);
        let h2 = p.square(&h);
        let h3 = p.mul(&h, &h2);
        let u = p.mul(&u1, &h2);
        let x = p.sub(&p.sub(&p.square(&r), &h3), &p.add(&u, &u));
        let y = p.sub(&p.mul(&r, &p.sub(&u, &x)), &p.mul(&s1, &h3));
        let z = p.mul(&h, &p.mul(&self.z, &q.z));
        let out = Self { x, y, z };
        let out = Self::select(&out, &self.double(c), h.eq(&V::zero()) & r.eq(&V::zero()));
        let out = Self::select(&out, self, q.z.eq(&V::zero()));
        Self::select(&out, q, self.z.eq(&V::zero()))
    }
    pub(crate) fn mul(&self, scalar: &V<N>, c: &Curve<N>) -> Self {
        let mut r = Self::identity(c);
        for i in (0..N * 64).rev() {
            let doubled = r.double(c);
            let added = doubled.add(self, c);
            r = Self::select(&doubled, &added, (scalar.0[i / 64] >> (i % 64)) & 1)
        }
        r
    }
    fn affine(&self, c: &Curve<N>) -> Result<(V<N>, V<N>), Error> {
        if self.z.eq(&V::zero()) == 1 {
            return Err(Error::InvalidPoint);
        }
        let inverse = c.p.invert(&self.z);
        let inv2 = c.p.square(&inverse);
        Ok((
            c.p.raw(&c.p.mul(&self.x, &inv2)),
            c.p.raw(&c.p.mul(&self.y, &c.p.mul(&inv2, &inverse))),
        ))
    }
    pub(crate) fn encode(&self, c: &Curve<N>, out: &mut [u8]) -> Result<(), Error> {
        if out.len() != 1 + N * 16 {
            return Err(Error::Length);
        }
        let (x, y) = self.affine(c)?;
        out[0] = 4;
        Mod::to_be(&x, &mut out[1..1 + N * 8]);
        Mod::to_be(&y, &mut out[1 + N * 8..]);
        Ok(())
    }
}
impl<const N: usize> Curve<N> {
    pub(crate) fn new(p: [u64; N], n: [u64; N], b: [u64; N], gx: [u64; N], gy: [u64; N]) -> Self {
        let p = Mod::new(p);
        let n = Mod::new(n);
        let b = p.encode_raw(&V(b));
        let gx = p.encode_raw(&V(gx));
        let gy = p.encode_raw(&V(gy));
        Self { p, n, b, gx, gy }
    }
    pub(crate) fn base(&self) -> Point<N> {
        Point {
            x: V(self.gx.0),
            y: V(self.gy.0),
            z: self.p.one(),
        }
    }
    pub(crate) fn decode(&self, encoded: &[u8]) -> Result<Point<N>, Error> {
        if encoded.len() != 1 + N * 16 {
            return Err(Error::Length);
        }
        if encoded[0] != 4 {
            return Err(Error::InvalidEncoding);
        }
        let x = Mod::from_be(&encoded[1..1 + N * 8]);
        let y = Mod::from_be(&encoded[1 + N * 8..]);
        if self.p.valid(&x) & self.p.valid(&y) != 1 {
            return Err(Error::InvalidPoint);
        }
        let x = self.p.encode_raw(&x);
        let y = self.p.encode_raw(&y);
        let rhs = self.p.add(
            &self.p.sub(
                &self.p.mul(&self.p.square(&x), &x),
                &self.p.mul(&self.p.small(3), &x),
            ),
            &self.b,
        );
        if self.p.square(&y).eq(&rhs) != 1 {
            return Err(Error::InvalidPoint);
        }
        Ok(Point {
            x,
            y,
            z: self.p.one(),
        })
    }
    pub(crate) fn scalar(&self, bytes: &[u8]) -> Result<V<N>, Error> {
        if bytes.len() != N * 8 {
            return Err(Error::Length);
        }
        let s = Mod::from_be(bytes);
        if self.n.valid(&s) & (1 ^ s.eq(&V::zero())) != 1 {
            return Err(Error::InvalidKey);
        }
        Ok(s)
    }
    pub(crate) fn agree(
        &self,
        scalar: &V<N>,
        point: &Point<N>,
        output: &mut [u8],
    ) -> Result<(), Error> {
        // Both curves have prime order and cofactor one; validated nonzero
        // scalars and finite peers cannot multiply to infinity.
        let (x, _) = point
            .mul(scalar, self)
            .affine(self)
            .expect("validated prime-order point and nonzero scalar");
        Mod::to_be(&x, output);
        Ok(())
    }
    pub(crate) fn verify(
        &self,
        point: &Point<N>,
        digest: &[u8],
        signature: &[u8],
    ) -> Result<(), Error> {
        if digest.len() != N * 8 {
            return Err(Error::Length);
        }
        let (r, s) = der::<N>(signature)?;
        let r = self
            .scalar(&r[..N * 8])
            .map_err(|_| Error::InvalidSignature)?;
        let s = self
            .scalar(&s[..N * 8])
            .map_err(|_| Error::InvalidSignature)?;
        let z = Mod::from_be(digest);
        let z = self.n.add(&V::zero(), &z);
        let inverse = self.n.invert(&self.n.encode_raw(&s));
        let u = self.n.raw(&self.n.mul(&self.n.encode_raw(&z), &inverse));
        let v = self.n.raw(&self.n.mul(&self.n.encode_raw(&r), &inverse));
        let result = self.base().mul(&u, self).add(&point.mul(&v, self), self);
        let (x, _) = result.affine(self).map_err(|_| Error::InvalidSignature)?;
        let x = self.n.add(&V::zero(), &x);
        if x.eq(&r) == 1 {
            Ok(())
        } else {
            Err(Error::InvalidSignature)
        }
    }
}
fn der<const N: usize>(input: &[u8]) -> Result<([u8; 48], [u8; 48]), Error> {
    // A two-integer P-384 signature is below 128 bytes: long lengths are invalid.
    if input.len() < 6 || input[0] != 0x30 || input[1] as usize != input.len() - 2 {
        return Err(Error::InvalidEncoding);
    }
    let mut cursor = 2;
    let mut integer = || -> Result<[u8; 48], Error> {
        if input.get(cursor) != Some(&2) {
            return Err(Error::InvalidEncoding);
        }
        let len = *input.get(cursor + 1).ok_or(Error::InvalidEncoding)? as usize;
        cursor += 2;
        let end = cursor.checked_add(len).ok_or(Error::InvalidEncoding)?;
        let mut bytes = input.get(cursor..end).ok_or(Error::InvalidEncoding)?;
        cursor = end;
        if bytes.is_empty() || bytes[0] & 128 != 0 {
            return Err(Error::InvalidEncoding);
        }
        if bytes[0] == 0 {
            if bytes.len() > 1 && bytes[1] & 128 == 0 {
                return Err(Error::InvalidEncoding);
            }
            bytes = &bytes[1..]
        }
        if bytes.len() > N * 8 {
            return Err(Error::InvalidSignature);
        }
        let mut result = [0; 48];
        result[N * 8 - bytes.len()..N * 8].copy_from_slice(bytes);
        Ok(result)
    };
    let r = integer()?;
    let s = integer()?;
    if cursor != input.len() {
        return Err(Error::InvalidEncoding);
    }
    Ok((r, s))
}
pub(crate) fn p256() -> Curve<4> {
    Curve::new(
        [
            0xffffffffffffffff,
            0x00000000ffffffff,
            0,
            0xffffffff00000001,
        ],
        [
            0xf3b9cac2fc632551,
            0xbce6faada7179e84,
            0xffffffffffffffff,
            0xffffffff00000000,
        ],
        [
            0x3bce3c3e27d2604b,
            0x651d06b0cc53b0f6,
            0xb3ebbd55769886bc,
            0x5ac635d8aa3a93e7,
        ],
        [
            0xf4a13945d898c296,
            0x77037d812deb33a0,
            0xf8bce6e563a440f2,
            0x6b17d1f2e12c4247,
        ],
        [
            0xcbb6406837bf51f5,
            0x2bce33576b315ece,
            0x8ee7eb4a7c0f9e16,
            0x4fe342e2fe1a7f9b,
        ],
    )
}
pub(crate) fn p384() -> Curve<6> {
    Curve::new(
        [
            0x00000000ffffffff,
            0xffffffff00000000,
            0xfffffffffffffffe,
            0xffffffffffffffff,
            0xffffffffffffffff,
            0xffffffffffffffff,
        ],
        [
            0xecec196accc52973,
            0x581a0db248b0a77a,
            0xc7634d81f4372ddf,
            0xffffffffffffffff,
            0xffffffffffffffff,
            0xffffffffffffffff,
        ],
        [
            0x2a85c8edd3ec2aef,
            0xc656398d8a2ed19d,
            0x0314088f5013875a,
            0x181d9c6efe814112,
            0x988e056be3f82d19,
            0xb3312fa7e23ee7e4,
        ],
        [
            0x3a545e3872760ab7,
            0x5502f25dbf55296c,
            0x59f741e082542a38,
            0x6e1d3b628ba79b98,
            0x8eb1c71ef320ad74,
            0xaa87ca22be8b0537,
        ],
        [
            0x7a431d7c90ea0e5f,
            0x0a60b1ce1d7e819d,
            0xe9da3113b5f0b8c0,
            0xf8f41dbd289a147c,
            0x5d9e98bf9292dc29,
            0x3617de4a96262c6f,
        ],
    )
}
