//! SEC 1 NIST P-256/P-384 arithmetic. ECDH uses fixed-width scalars and a
//! double-and-add schedule with complete masked handling of exceptional points.
//! SEC 1 decoding, DER decoding, ECDSA verification and key-validity refusals
//! depend on public data. Only uncompressed SEC 1 points are accepted.
#![allow(
    clippy::needless_range_loop,
    reason = "masked selects index parallel fixed-size arrays, as `montgomery` does"
)]

use crate::{
    Error,
    montgomery::{self, Modulus, Residue, below, from_be, to_be},
    secret::wipe64,
};

/// A short Weierstrass curve y^2 = x^3 - 3x + b of prime order and cofactor
/// one, with its constants in Montgomery form at compile time.
pub(crate) trait Curve<const N: usize>: 'static {
    type Field: Modulus<N>;
    type Order: Modulus<N>;
    const B: [u64; N];
    const GX: [u64; N];
    const GY: [u64; N];
}
type Coord<C, const N: usize> = Residue<<C as Curve<N>>::Field, N>;
type ModN<C, const N: usize> = Residue<<C as Curve<N>>::Order, N>;

/// A scalar of curve `C` below n, erased on drop. A secret one is never zero.
pub(crate) struct Scalar<C: Curve<N>, const N: usize>([u64; N], core::marker::PhantomData<C>);
impl<C: Curve<N>, const N: usize> Drop for Scalar<C, N> {
    fn drop(&mut self) {
        wipe64(&mut self.0)
    }
}
impl<C: Curve<N>, const N: usize> Scalar<C, N> {
    /// One when 0 < x < n, otherwise zero; the time does not depend on x.
    fn valid(x: &[u64; N]) -> u64 {
        let mut d = 0;
        for word in x {
            d |= word
        }
        below(x, &C::Order::P) & crate::ct::nonzero(d)
    }
    /// Big-endian bytes, 8N of them. Only validity depends on their value.
    pub(crate) fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() != N * 8 {
            return Err(Error::Length);
        }
        let s = Self(from_be(bytes), core::marker::PhantomData);
        if Self::valid(&s.0) != 1 {
            return Err(Error::InvalidKey);
        }
        Ok(s)
    }
    /// The first valid candidate of `candidates`, 8N big-endian bytes each, on
    /// a schedule that discloses only whether any is valid.
    pub(crate) fn first_valid(candidates: &[u8]) -> Result<Self, Error> {
        assert_eq!(candidates.len() % (N * 8), 0);
        let mut scalar = Self([0; N], core::marker::PhantomData);
        let mut selected = 0;
        for i in 0..candidates.len() / (N * 8) {
            let bytes = &candidates[i * N * 8..(i + 1) * N * 8];
            let mut candidate = from_be::<N>(bytes);
            let take = crate::ct::mask(Self::valid(&candidate) & (1 ^ selected));
            for i in 0..N {
                scalar.0[i] = (scalar.0[i] & !take) | (candidate[i] & take);
            }
            selected |= take & 1;
            wipe64(&mut candidate);
        }
        if selected != 1 {
            return Err(Error::Entropy);
        }
        Ok(scalar)
    }
    fn bit(&self, i: usize) -> u64 {
        (self.0[i / 64] >> (i % 64)) & 1
    }
}

/// Jacobian coordinates (X : Y : Z) for x = X/Z^2, y = Y/Z^3; Z = 0 is the
/// point at infinity.
pub(crate) struct Point<C: Curve<N>, const N: usize> {
    x: Coord<C, N>,
    y: Coord<C, N>,
    z: Coord<C, N>,
}
impl<C: Curve<N>, const N: usize> Clone for Point<C, N> {
    fn clone(&self) -> Self {
        Self { x: self.x.clone(), y: self.y.clone(), z: self.z.clone() }
    }
}
impl<C: Curve<N>, const N: usize> Point<C, N> {
    fn identity() -> Self {
        Self {
            x: Residue::zero(),
            y: Residue::one(),
            z: Residue::zero(),
        }
    }
    pub(crate) fn base() -> Self {
        Self {
            x: Residue::from_montgomery(C::GX),
            y: Residue::from_montgomery(C::GY),
            z: Residue::one(),
        }
    }
    fn select(a: &Self, b: &Self, bit: u64) -> Self {
        Self {
            x: Residue::select(&a.x, &b.x, bit),
            y: Residue::select(&a.y, &b.y, bit),
            z: Residue::select(&a.z, &b.z, bit),
        }
    }
    /// "dbl-2001-b" for a = -3 (Bernstein and Lange, Explicit-Formulas
    /// Database), 3M + 5S. Infinity doubles to Z = 0, and no point of a
    /// prime-order curve has y = 0.
    fn double(&self) -> Self {
        let delta = self.z.square();
        let gamma = self.y.square();
        let beta = self.x.mul(&gamma);
        let t = self.x.sub(&delta).mul(&self.x.add(&delta));
        let alpha = t.double().add(&t);
        let beta4 = beta.double().double();
        let x = alpha.square().sub(&beta4.double());
        let z = self.y.add(&self.z).square().sub(&gamma).sub(&delta);
        let gamma2 = gamma.square().double();
        let y = alpha.mul(&beta4.sub(&x)).sub(&gamma2.double().double());
        Self { x, y, z }
    }
    fn add(&self, q: &Self) -> Self {
        let z1 = self.z.square();
        let z2 = q.z.square();
        let u1 = self.x.mul(&z2);
        let u2 = q.x.mul(&z1);
        let s1 = self.y.mul(&q.z.mul(&z2));
        let s2 = q.y.mul(&self.z.mul(&z1));
        let h = u2.sub(&u1);
        let r = s2.sub(&s1);
        let h2 = h.square();
        let h3 = h.mul(&h2);
        let u = u1.mul(&h2);
        let x = r.square().sub(&h3).sub(&u.double());
        let y = r.mul(&u.sub(&x)).sub(&s1.mul(&h3));
        let z = h.mul(&self.z.mul(&q.z));
        let out = Self { x, y, z };
        let out = Self::select(&out, &self.double(), h.is_zero() & r.is_zero());
        let out = Self::select(&out, self, q.z.is_zero());
        Self::select(&out, q, self.z.is_zero())
    }
    /// scalar * self on a fixed schedule. The accumulator is the operation's
    /// secret state, erased when it returns; the result is the caller's.
    pub(crate) fn mul(&self, scalar: &Scalar<C, N>) -> Self {
        let mut r = Self::identity();
        for i in (0..N * 64).rev() {
            let doubled = r.double();
            let added = doubled.add(self);
            r = Self::select(&doubled, &added, scalar.bit(i))
        }
        r
    }
    fn affine(&self) -> Result<([u64; N], [u64; N]), Error> {
        if self.z.is_zero() == 1 {
            return Err(Error::InvalidPoint);
        }
        let inverse = self.z.invert();
        let inv2 = inverse.square();
        Ok((
            self.x.mul(&inv2).integer(),
            self.y.mul(&inv2.mul(&inverse)).integer(),
        ))
    }
    /// The uncompressed SEC 1 encoding, 1 + 16N bytes.
    pub(crate) fn encode(&self, out: &mut [u8]) -> Result<(), Error> {
        if out.len() != 1 + N * 16 {
            return Err(Error::Length);
        }
        let (x, y) = self.affine()?;
        out[0] = 4;
        to_be(&x, &mut out[1..1 + N * 8]);
        to_be(&y, &mut out[1 + N * 8..]);
        Ok(())
    }
    /// An uncompressed SEC 1 point on the curve.
    pub(crate) fn decode(encoded: &[u8]) -> Result<Self, Error> {
        if encoded.len() != 1 + N * 16 {
            return Err(Error::Length);
        }
        if encoded[0] != 4 {
            return Err(Error::InvalidEncoding);
        }
        let x = Coord::<C, N>::from_integer(&from_be(&encoded[1..1 + N * 8]));
        let y = Coord::<C, N>::from_integer(&from_be(&encoded[1 + N * 8..]));
        let (Some(x), Some(y)) = (x, y) else {
            return Err(Error::InvalidPoint);
        };
        let rhs = x
            .square()
            .mul(&x)
            .sub(&x.double().add(&x))
            .add(&Residue::from_montgomery(C::B));
        if y.square().eq(&rhs) != 1 {
            return Err(Error::InvalidPoint);
        }
        Ok(Point {
            x,
            y,
            z: Residue::one(),
        })
    }
    /// The x-coordinate of scalar * self into `output`, 8N bytes.
    pub(crate) fn agree(&self, scalar: &Scalar<C, N>, output: &mut [u8]) {
        // Both curves have prime order and cofactor one; validated nonzero
        // scalars and finite peers cannot multiply to infinity.
        let (x, _) = self
            .mul(scalar)
            .affine()
            .expect("validated prime-order point and nonzero scalar");
        to_be(&x, output);
    }
    /// ECDSA verification of a DER signature over an 8N-byte digest. All
    /// inputs are public.
    pub(crate) fn verify(&self, digest: &[u8], signature: &[u8]) -> Result<(), Error> {
        if digest.len() != N * 8 {
            return Err(Error::Length);
        }
        let (r, s) = der::<N>(signature)?;
        let r = Scalar::<C, N>::from_bytes(&r[..N * 8]).map_err(|_| Error::InvalidSignature)?;
        let s = Scalar::<C, N>::from_bytes(&s[..N * 8]).map_err(|_| Error::InvalidSignature)?;
        let z = ModN::<C, N>::from_wide(&from_be(digest));
        let inverse = ModN::<C, N>::from_wide(&s.0).invert();
        let u = Scalar(z.mul(&inverse).integer(), core::marker::PhantomData);
        let v = Scalar(ModN::<C, N>::from_wide(&r.0).mul(&inverse).integer(), core::marker::PhantomData);
        let result = Self::base().mul(&u).add(&self.mul(&v));
        let (x, _) = result.affine().map_err(|_| Error::InvalidSignature)?;
        let x = ModN::<C, N>::from_wide(&x).integer();
        if x == r.0 {
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
macro_rules! modulus {
    ($name:ident, $n:literal, $p:expr) => {
        pub(crate) struct $name;
        impl Modulus<$n> for $name {
            const P: [u64; $n] = $p;
        }
    };
}
modulus!(P256Field, 4, [0xffffffffffffffff, 0x00000000ffffffff, 0, 0xffffffff00000001]);
modulus!(P256Order, 4, [0xf3b9cac2fc632551, 0xbce6faada7179e84, 0xffffffffffffffff, 0xffffffff00000000]);
modulus!(
    P384Field,
    6,
    [
        0x00000000ffffffff,
        0xffffffff00000000,
        0xfffffffffffffffe,
        0xffffffffffffffff,
        0xffffffffffffffff,
        0xffffffffffffffff,
    ]
);
modulus!(
    P384Order,
    6,
    [
        0xecec196accc52973,
        0x581a0db248b0a77a,
        0xc7634d81f4372ddf,
        0xffffffffffffffff,
        0xffffffffffffffff,
        0xffffffffffffffff,
    ]
);

/// P-256 (FIPS 186-5, SP 800-186 section 3.2.1.3).
pub(crate) struct P256;
impl Curve<4> for P256 {
    type Field = P256Field;
    type Order = P256Order;
    const B: [u64; 4] = montgomery::encode(
        &[0x3bce3c3e27d2604b, 0x651d06b0cc53b0f6, 0xb3ebbd55769886bc, 0x5ac635d8aa3a93e7],
        &P256Field::P,
    );
    const GX: [u64; 4] = montgomery::encode(
        &[0xf4a13945d898c296, 0x77037d812deb33a0, 0xf8bce6e563a440f2, 0x6b17d1f2e12c4247],
        &P256Field::P,
    );
    const GY: [u64; 4] = montgomery::encode(
        &[0xcbb6406837bf51f5, 0x2bce33576b315ece, 0x8ee7eb4a7c0f9e16, 0x4fe342e2fe1a7f9b],
        &P256Field::P,
    );
}

/// P-384 (FIPS 186-5, SP 800-186 section 3.2.1.4).
pub(crate) struct P384;
impl Curve<6> for P384 {
    type Field = P384Field;
    type Order = P384Order;
    const B: [u64; 6] = montgomery::encode(
        &[
            0x2a85c8edd3ec2aef,
            0xc656398d8a2ed19d,
            0x0314088f5013875a,
            0x181d9c6efe814112,
            0x988e056be3f82d19,
            0xb3312fa7e23ee7e4,
        ],
        &P384Field::P,
    );
    const GX: [u64; 6] = montgomery::encode(
        &[
            0x3a545e3872760ab7,
            0x5502f25dbf55296c,
            0x59f741e082542a38,
            0x6e1d3b628ba79b98,
            0x8eb1c71ef320ad74,
            0xaa87ca22be8b0537,
        ],
        &P384Field::P,
    );
    const GY: [u64; 6] = montgomery::encode(
        &[
            0x7a431d7c90ea0e5f,
            0x0a60b1ce1d7e819d,
            0xe9da3113b5f0b8c0,
            0xf8f41dbd289a147c,
            0x5d9e98bf9292dc29,
            0x3617de4a96262c6f,
        ],
        &P384Field::P,
    );
}
