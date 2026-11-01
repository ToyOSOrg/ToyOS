//! RFC 8032 pure Ed25519, without a context or prehash. Verification's
//! rules are [`PublicKey::from_bytes`]'s and [`PublicKey::verify`]'s.
//! Verification's inputs are all public: it decodes, recodes and
//! multiplies in variable time, through [`PublicScalar`] only. Signing
//! (feature `sign`) and key generation multiply the base point by a
//! fixed-base comb on a fixed schedule: every table entry of a row is read
//! and the choice is masked, so neither branches nor addresses depend on
//! the secret scalar.

use crate::{
    Error, ct,
    field25519::{Loose, Multiply, Tight},
    hash::Sha512,
    scalar25519::PublicScalar,
    secret::Workspace,
};
#[cfg(feature = "sign")]
use crate::{Random, scalar25519::S, secret::Secret};
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

/// The canonical encodings of the eight points of order dividing 8, which
/// [`PublicKey::from_bytes`] refuses.
const SMALL_ORDER: [[u8; 32]; 8] = [
    hex("0000000000000000000000000000000000000000000000000000000000000000"),
    hex("0000000000000000000000000000000000000000000000000000000000000080"),
    hex("0100000000000000000000000000000000000000000000000000000000000000"),
    hex("26e8958fc2b227b045c3f489f2ef98f0d5dfac05d3c63339b13802886d53fc05"),
    hex("26e8958fc2b227b045c3f489f2ef98f0d5dfac05d3c63339b13802886d53fc85"),
    hex("c7176a703d4dd84fba3c0b760d10670f2a2053fa2c39ccc64ec7fd7792ac037a"),
    hex("c7176a703d4dd84fba3c0b760d10670f2a2053fa2c39ccc64ec7fd7792ac03fa"),
    hex("ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f"),
];

const fn hex(s: &str) -> [u8; 32] {
    let s = s.as_bytes();
    let mut out = [0; 32];
    let mut i = 0;
    while i < 32 {
        out[i] = nibble(s[2 * i]) << 4 | nibble(s[2 * i + 1]);
        i += 1;
    }
    out
}

const fn nibble(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        _ => c - b'a' + 10,
    }
}

/// The width of B's recoding in verification: digits below 2^(W_B - 1), so
/// its table holds the 2^(W_B - 2) odd multiples B, 3B, 5B, ...
const W_B: u32 = 7;
/// The width of the public key's recoding, whose 8 odd multiples each
/// verification builds.
const W_A: u32 = 5;

/// The odd multiples of B in affine form, built at compile time.
static B_ODD: [Niels; 1 << (W_B - 2)] = Niels::odd_multiples_of_b();

/// Extended coordinates (X : Y : Z : T) with x = X/Z, y = Y/Z, xy = T/Z
/// (Hisil, Wong, Carter and Dawson, "Twisted Edwards Curves Revisited",
/// ASIACRYPT 2008).
#[derive(Clone, Copy)]
struct Point {
    x: Tight,
    y: Tight,
    z: Tight,
    t: Tight,
}

/// Projective coordinates (X : Y : Z), the doubling chain's form.
#[derive(Clone, Copy)]
struct Projective {
    x: Tight,
    y: Tight,
    z: Tight,
}

/// A completed point ((E : G), (H : F)) with x = E/G and y = H/F: what an
/// addition or doubling yields before its last multiplications.
struct Completed {
    e: Loose,
    f: Loose,
    g: Loose,
    h: Loose,
}

/// (Y + X, Y - X, 2Z, 2dT) of a point, an addition's second operand.
#[derive(Clone, Copy)]
struct Cached {
    y_plus_x: Tight,
    y_minus_x: Tight,
    z2: Tight,
    t2d: Tight,
}

/// (y + x, y - x, 2dxy) of an affine point.
#[derive(Clone, Copy)]
struct Niels {
    y_plus_x: Tight,
    y_minus_x: Tight,
    xy2d: Tight,
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
    #[cfg(test)]
    fn base() -> Self {
        Self {
            x: B_X,
            y: B_Y,
            z: Tight::ONE,
            t: B_T,
        }
    }
    /// RFC 8032 section 5.1.3: x = u v^3 (u v^7)^((p - 5) / 8) for u = y^2 - 1
    /// and v = d y^2 + 1, one exponentiation. Public input, variable time.
    fn decode(input: &[u8; 32]) -> Result<Self, Error> {
        let mut b = *input;
        let sign = u64::from(b[31] >> 7);
        b[31] &= 127;
        let y = Tight::from_bytes(&b);
        if b != y.bytes() {
            return Err(Error::InvalidPoint);
        }
        let y2 = y.square();
        let u = y2.sub(&Tight::ONE).carry();
        let v = D.mul(&y2).add(&Tight::ONE).carry();
        let v3 = v.square().mul(&v);
        let v7 = v3.square().mul(&v);
        let x = u.mul(&v3).mul(&u.mul(&v7).pow_p58());
        let vxx = v.mul(&x.square());
        let x = if vxx.eq(&u) == 1 {
            x
        } else if vxx.eq(&u.neg().carry()) == 1 {
            x.mul(&SQRT_M1)
        } else {
            return Err(Error::InvalidPoint);
        };
        if x.is_zero() == 1 && sign == 1 {
            return Err(Error::InvalidPoint);
        }
        let x = if x.is_negative() == sign { x } else { x.neg().carry() };
        Ok(Self {
            x,
            y,
            z: Tight::ONE,
            t: x.mul(&y),
        })
    }
    #[cfg(feature = "sign")]
    fn encode(&self) -> [u8; 32] {
        Projective {
            x: self.x,
            y: self.y,
            z: self.z,
        }
        .encode()
    }
    fn cached(&self) -> Cached {
        Cached {
            y_plus_x: self.y.add(&self.x).carry(),
            y_minus_x: self.y.sub(&self.x).carry(),
            z2: self.z.add(&self.z).carry(),
            t2d: self.t.mul(&D2),
        }
    }
    fn add_cached(&self, q: &Cached) -> Completed {
        let a = self.y.sub(&self.x).mul(&q.y_minus_x);
        let b = self.y.add(&self.x).mul(&q.y_plus_x);
        let c = self.t.mul(&q.t2d);
        let d = self.z.mul(&q.z2);
        Completed::from_abcd(&a, &b, &c, &d)
    }
    fn sub_cached(&self, q: &Cached) -> Completed {
        let a = self.y.sub(&self.x).mul(&q.y_plus_x);
        let b = self.y.add(&self.x).mul(&q.y_minus_x);
        let c = self.t.mul(&q.t2d).neg().carry();
        let d = self.z.mul(&q.z2);
        Completed::from_abcd(&a, &b, &c, &d)
    }
    fn add_niels(&self, q: &Niels) -> Completed {
        let a = self.y.sub(&self.x).mul(&q.y_minus_x);
        let b = self.y.add(&self.x).mul(&q.y_plus_x);
        let c = self.t.mul(&q.xy2d);
        let d = self.z.add(&self.z).carry();
        Completed::from_abcd(&a, &b, &c, &d)
    }
    fn sub_niels(&self, q: &Niels) -> Completed {
        let a = self.y.sub(&self.x).mul(&q.y_plus_x);
        let b = self.y.add(&self.x).mul(&q.y_minus_x);
        let c = self.t.mul(&q.xy2d).neg().carry();
        let d = self.z.add(&self.z).carry();
        Completed::from_abcd(&a, &b, &c, &d)
    }
    fn double(&self) -> Self {
        Projective {
            x: self.x,
            y: self.y,
            z: self.z,
        }
        .double()
        .point()
    }
}

impl Completed {
    /// E = B - A, F = D - C, G = D + C, H = B + A (RFC 8032 section 5.1.4).
    fn from_abcd(a: &Tight, b: &Tight, c: &Tight, d: &Tight) -> Self {
        Self {
            e: b.sub(a),
            f: d.sub(c),
            g: d.add(c),
            h: b.add(a),
        }
    }
    fn point(&self) -> Point {
        Point {
            x: self.e.mul(&self.f),
            y: self.g.mul(&self.h),
            z: self.f.mul(&self.g),
            t: self.e.mul(&self.h),
        }
    }
    fn projective(&self) -> Projective {
        Projective {
            x: self.e.mul(&self.f),
            y: self.g.mul(&self.h),
            z: self.f.mul(&self.g),
        }
    }
}

impl Projective {
    const IDENTITY: Self = Self {
        x: Tight::ZERO,
        y: Tight::ONE,
        z: Tight::ONE,
    };
    /// RFC 8032 section 5.1.4's doubling, which reads no T.
    fn double(&self) -> Completed {
        let a = self.x.square();
        let b = self.y.square();
        let zz = self.z.square();
        let c = zz.add(&zz).carry();
        let h = a.add(&b).carry();
        let g = a.sub(&b).carry();
        Completed {
            e: h.sub(&self.x.add(&self.y).square()),
            f: c.add(&g),
            g: g.widen(),
            h: h.widen(),
        }
    }
    fn encode(&self) -> [u8; 32] {
        let inverse = self.z.invert();
        let x = self.x.mul(&inverse);
        let mut y = self.y.mul(&inverse).bytes();
        y[31] |= (x.is_negative() as u8) << 7;
        y
    }
}

impl Niels {
    /// B, 3B, 5B, ...
    const fn odd_multiples_of_b<const N: usize>() -> [Niels; N] {
        let base = [B_X, B_Y, Tight::ONE, B_T];
        let twice = add_const(&base, &base);
        let mut points = [base; N];
        let mut i = 1;
        while i < N {
            points[i] = add_const(&points[i - 1], &twice);
            i += 1;
        }
        normalize(&points)
    }
}

/// RFC 8032 section 5.1.4's addition of extended points, for constants.
const fn add_const(p: &[Tight; 4], q: &[Tight; 4]) -> [Tight; 4] {
    let [x1, y1, z1, t1] = p;
    let [x2, y2, z2, t2] = q;
    let a = y1.sub(x1).carry().times(&y2.sub(x2).carry());
    let b = y1.add(x1).carry().times(&y2.add(x2).carry());
    let c = t1.times(t2).times(&D2);
    let zz = z1.times(z2);
    let d = zz.add(&zz).carry();
    let e = b.sub(&a).carry();
    let f = d.sub(&c).carry();
    let g = d.add(&c).carry();
    let h = b.add(&a).carry();
    [e.times(&f), g.times(&h), f.times(&g), e.times(&h)]
}

/// Extended points in affine Niels form, with one inversion for all of them
/// (Montgomery's simultaneous-inversion trick).
const fn normalize<const N: usize>(points: &[[Tight; 4]; N]) -> [Niels; N] {
    // prefix[i] = z_0 z_1 ... z_i.
    let mut prefix = [Tight::ONE; N];
    prefix[0] = points[0][2];
    let mut i = 1;
    while i < N {
        prefix[i] = prefix[i - 1].times(&points[i][2]);
        i += 1;
    }
    let mut inverse = prefix[N - 1].inverted();
    let mut table = [Niels::IDENTITY; N];
    let mut i = N;
    while i > 0 {
        i -= 1;
        let z_inverse = if i == 0 { inverse } else { inverse.times(&prefix[i - 1]) };
        inverse = inverse.times(&points[i][2]);
        let x = points[i][0].times(&z_inverse);
        let y = points[i][1].times(&z_inverse);
        table[i] = Niels {
            y_plus_x: y.add(&x).carry(),
            y_minus_x: y.sub(&x).carry(),
            xy2d: x.times(&y).times(&D2),
        };
    }
    table
}

const fn flatten(rows: &[[[Tight; 4]; 8]; 32]) -> [[Tight; 4]; 256] {
    let mut flat = [rows[0][0]; 256];
    let mut n = 0;
    while n < 256 {
        flat[n] = rows[n / 8][n % 8];
        n += 1;
    }
    flat
}

/// The fixed-base comb's table: row i holds j 16^(2i) B for j = 1..=8, in
/// affine form, built at compile time (Bernstein, Duif, Lange, Schwabe and
/// Yang, "High-speed high-security signatures", J. Cryptogr. Eng. 2012,
/// section 4).
static COMB: [[Niels; 8]; 32] = Niels::comb_table();

impl Niels {
    const IDENTITY: Self = Self {
        y_plus_x: Tight::ONE,
        y_minus_x: Tight::ONE,
        xy2d: Tight::ZERO,
    };

    const fn comb_table() -> [[Niels; 8]; 32] {
        let base = [B_X, B_Y, Tight::ONE, B_T];
        let mut points = [[base; 8]; 32];
        let mut row = base;
        let mut i = 0;
        while i < 32 {
            points[i][0] = row;
            let mut j = 1;
            while j < 8 {
                points[i][j] = add_const(&points[i][j - 1], &row);
                j += 1;
            }
            // The next row's base is 16^2 times this one's: eight doublings.
            let mut k = 0;
            while k < 8 {
                row = add_const(&row, &row);
                k += 1;
            }
            i += 1;
        }
        let flat = normalize::<256>(&flatten(&points));
        let mut table = [[Self::IDENTITY; 8]; 32];
        let mut n = 0;
        while n < 256 {
            table[n / 8][n % 8] = flat[n];
            n += 1;
        }
        table
    }

    /// row[|d| - 1], negated when d < 0, or the identity when d = 0. Every
    /// entry is read and the choice is masked, so neither the time nor the
    /// addresses depend on d.
    fn select(row: &[Niels; 8], d: i8) -> Niels {
        let negative = u64::from((d as u8) >> 7);
        let magnitude = ((d ^ (0i8.wrapping_sub(negative as i8))).wrapping_add(negative as i8)) as u64;
        let mut chosen = Self::IDENTITY;
        for (j, entry) in row.iter().enumerate() {
            let take = 1 ^ ct::nonzero(magnitude ^ (j as u64 + 1));
            chosen = Self {
                y_plus_x: Tight::select(&chosen.y_plus_x, &entry.y_plus_x, take),
                y_minus_x: Tight::select(&chosen.y_minus_x, &entry.y_minus_x, take),
                xy2d: Tight::select(&chosen.xy2d, &entry.xy2d, take),
            };
        }
        // -(x, y) = (-x, y): exchange y + x with y - x and negate 2dxy.
        let mask = ct::mask(negative);
        let (mut y_plus_x, mut y_minus_x) = (chosen.y_plus_x, chosen.y_minus_x);
        Tight::swap(&mut y_plus_x, &mut y_minus_x, mask);
        Self {
            y_plus_x,
            y_minus_x,
            xy2d: Tight::select(&chosen.xy2d, &chosen.xy2d.neg().carry(), negative),
        }
    }
}

/// a = sum of e[i] 16^i with every e[i] in [-8, 8], for a below 2^255, by
/// arithmetic alone: each nibble is moved into [-8, 8) and the rest carried
/// up, and the top digit, at most 7 plus a carry, stays at most 8.
fn recode(a: &[u8; 32], e: &mut [i8; 64]) {
    for i in 0..32 {
        e[2 * i] = (a[i] & 15) as i8;
        e[2 * i + 1] = (a[i] >> 4) as i8;
    }
    // Wrapping operations: every value stays within [-8, 16], and the shipped
    // profile's overflow checks would branch on the secret digits.
    let mut carry = 0i8;
    for digit in &mut e[..63] {
        *digit = digit.wrapping_add(carry);
        carry = digit.wrapping_add(8) >> 4;
        *digit = digit.wrapping_sub(carry << 4);
    }
    e[63] = e[63].wrapping_add(carry);
}

/// The X25519 public key of a clamped scalar s: u = (1 + y) / (1 - y) of the
/// Edwards point [s]B, the birational map between the curves (RFC 7748
/// section 4.1), which takes B to the base point u = 9.
pub(crate) fn x25519_public(s: &[u8; 32]) -> [u8; 32] {
    let p = base_mul(s);
    // A clamped s is a multiple of 8 below 2^255 with bit 254 set, so it is
    // no multiple of L and [s]B is not the identity: Z - Y is nonzero.
    p.z.add(&p.y).mul(&p.z.sub(&p.y).carry().invert()).bytes()
}

/// [a]B for a 32-byte little-endian a below 2^255, on a fixed schedule: the
/// 64 signed radix-16 digits of a, 64 masked table reads and mixed
/// additions, and 4 doublings. The digits are the operation's secret state,
/// erased when it returns.
fn base_mul(a: &[u8; 32]) -> Point {
    let mut digits = Workspace([0i8; 64]);
    let e = &mut digits.0;
    recode(a, e);
    let mut r = Point::identity();
    for i in 0..32 {
        r = r.add_niels(&Niels::select(&COMB[i], e[2 * i + 1])).point();
    }
    let mut p = Projective {
        x: r.x,
        y: r.y,
        z: r.z,
    };
    for _ in 0..3 {
        p = p.double().projective();
    }
    r = p.double().point();
    for i in 0..32 {
        r = r.add_niels(&Niels::select(&COMB[i], e[2 * i])).point();
    }
    r
}

/// [s]B - [k]A by one doubling chain over both scalars' NAFs (Straus's
/// method): B's odd multiples from the static table, A's built here.
fn verification_point(s: &PublicScalar, k: &PublicScalar, a: &Point) -> Projective {
    let minus_a = Point {
        x: a.x.neg().carry(),
        y: a.y,
        z: a.z,
        t: a.t.neg().carry(),
    };
    let twice = minus_a.double().cached();
    let mut odd = [minus_a.cached(); 1 << (W_A - 2)];
    let mut p = minus_a;
    for entry in odd.iter_mut().skip(1) {
        p = p.add_cached(&twice).point();
        *entry = p.cached();
    }
    let (ds, dk) = (s.naf(W_B), k.naf(W_A));
    let Some(top) = (0..256).rev().find(|&i| ds[i] != 0 || dk[i] != 0) else {
        return Projective::IDENTITY;
    };
    let mut r = Projective::IDENTITY;
    for i in (0..=top).rev() {
        let mut t = r.double();
        if dk[i] > 0 {
            t = t.point().add_cached(&odd[dk[i] as usize / 2]);
        } else if dk[i] < 0 {
            t = t.point().sub_cached(&odd[(-dk[i]) as usize / 2]);
        }
        if ds[i] > 0 {
            t = t.point().add_niels(&B_ODD[ds[i] as usize / 2]);
        } else if ds[i] < 0 {
            t = t.point().sub_niels(&B_ODD[(-ds[i]) as usize / 2]);
        }
        r = t.projective();
    }
    r
}

pub struct PublicKey {
    encoded: [u8; 32],
    point: Point,
}
impl PublicKey {
    /// A key that decodes under RFC 8032 section 5.1.3, which refuses y of p
    /// or more, a y with no x and x = 0 with the sign bit set
    /// ([`Error::InvalidPoint`]), and that is not of order dividing 8
    /// ([`Error::InvalidKey`]). RFC 8032 does not refuse the latter, but no
    /// one holds a secret for it: R = [S]B verifies under one whenever 8
    /// divides k. Mixed-order keys are taken, as RFC 8032 takes them; only
    /// the holder of their scalar can sign for them.
    pub fn from_bytes(encoded: [u8; 32]) -> Result<Self, Error> {
        let point = Point::decode(&encoded)?;
        // A decoded encoding is canonical, so a small-order point is one of
        // the eight.
        if SMALL_ORDER.contains(&encoded) {
            return Err(Error::InvalidKey);
        }
        Ok(Self { encoded, point })
    }
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.encoded
    }
    /// RFC 8032 section 5.1.7 for a 64-byte R || S, with S below L, and the
    /// equation [S]B = R + [k mod L]A for k = SHA-512(R || A || M) over the
    /// bytes received, reduced as section 6's reference `verify` reduces it.
    /// It accepts when [S]B - [k mod L]A encodes to R's bytes; a
    /// non-canonical R, one off the curve or a negative zero can never equal
    /// an encoding, so R decodes as section 5.1.3 requires. Section 5.1.7's
    /// literal equation takes k unreduced, which differs only for a
    /// mixed-order A. Times 8 this equation is the cofactored one, which
    /// accepts more where R or A has a nonzero torsion component; RFC 8032's
    /// key generation and signing make neither. An R of order dividing 8 is
    /// taken, as RFC 8032 takes it: under a prime-order A it is the identity
    /// with S = k a mod L, which only the key's holder can compute, and RFC
    /// 8032 signing makes it whenever r is a multiple of L.
    pub fn verify(&self, message: &[u8], signature: &[u8]) -> Result<(), Error> {
        if signature.len() != 64 {
            return Err(Error::Length);
        }
        let r_bytes: &[u8; 32] = signature[..32].try_into().unwrap();
        let s_bytes: &[u8; 32] = signature[32..].try_into().unwrap();
        let s = PublicScalar::canonical(s_bytes).ok_or(Error::InvalidSignature)?;
        let mut h = Sha512::new();
        h.update(r_bytes)?;
        h.update(&self.encoded)?;
        h.update(message)?;
        let k = PublicScalar::reduce(&h.finalize());
        if ct::equal(&verification_point(&s, &k, &self.point).encode(), r_bytes) {
            Ok(())
        } else {
            Err(Error::InvalidSignature)
        }
    }
}
#[cfg(feature = "sign")]
pub struct SigningKey {
    /// The clamped secret scalar, reduced mod L once.
    scalar: S,
    prefix: Secret<32>,
    public: PublicKey,
}
#[cfg(feature = "sign")]
impl SigningKey {
    pub fn from_seed(seed: [u8; 32]) -> Self {
        let _protection = crate::timing::protect();
        let seed = Secret(seed);
        let mut expanded = Secret(Sha512::digest(&seed.0).expect("fixed 32-byte seed"));
        expanded.0[0] &= 248;
        expanded.0[31] = (expanded.0[31] & 63) | 64;
        let scalar = S::reduce32(expanded.0[..32].try_into().unwrap());
        let prefix = Secret(expanded.0[32..].try_into().unwrap());
        // B has order L, so the reduced scalar gives the clamped one's point.
        let point = base_mul(&Secret(scalar.bytes()).0);
        let public = PublicKey {
            encoded: point.encode(),
            point,
        };
        Self { scalar, prefix, public }
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
        let _protection = crate::timing::protect();
        let mut h = Sha512::new();
        h.update(&self.prefix.0)?;
        h.update(message)?;
        let digest = Secret(h.finalize());
        let r = S::reduce(&digest.0);
        let r_bytes = Secret(r.bytes());
        let encoded = base_mul(&r_bytes.0).encode();
        let mut h = Sha512::new();
        h.update(&encoded)?;
        h.update(self.public.as_bytes())?;
        h.update(message)?;
        let k = Secret(h.finalize());
        let k = S::reduce(&k.0);
        let s = k.mul_add(&self.scalar, &r);
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

    /// The eight encodings are distinct, decode, and are each killed by 8;
    /// the torsion subgroup has exactly eight points.
    #[test]
    fn small_order_encodings_are_the_eight_torsion_points() {
        for (i, encoding) in SMALL_ORDER.iter().enumerate() {
            assert!(!SMALL_ORDER[..i].contains(encoding));
            let p = Point::decode(encoding).unwrap();
            let eight = p.double().double().double();
            assert_eq!(eight.x.is_zero(), 1);
            assert_eq!(eight.y.eq(&eight.z), 1);
        }
    }

    /// Encodings of y in [p, 2^255), the noncanonical ones, are refused
    /// whether or not y mod p is on the curve, with either sign; so is
    /// negative zero.
    #[test]
    fn noncanonical_encodings_and_negative_zero_are_refused() {
        let p = hex("edffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f");
        let mut on_curve = 0;
        for k in 0..19u8 {
            let mut y = p;
            y[0] += k;
            let mut reduced = [0u8; 32];
            reduced[0] = k;
            for sign in [0, 0x80] {
                let (mut a, mut b) = (y, reduced);
                a[31] |= sign;
                b[31] |= sign;
                assert!(PublicKey::from_bytes(a).is_err(), "y = p + {k}, sign {sign}");
                on_curve += usize::from(Point::decode(&b).is_ok());
            }
        }
        // Some of them decode once reduced, so the refusal is the encoding's.
        assert!(on_curve > 0);
        for zero in ["0100000000000000000000000000000000000000000000000000000000000080", "ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"] {
            assert!(Point::decode(&hex(zero)).is_err(), "{zero}");
            assert!(PublicKey::from_bytes(hex(zero)).is_err(), "{zero}");
        }
    }

    /// The comb's rows are j 16^(2i) B, against run-time additions.
    #[test]
    fn comb_table_holds_its_multiples() {
        let mut row = Point::base();
        for (i, entries) in COMB.iter().enumerate() {
            let mut p = row;
            for (j, entry) in entries.iter().enumerate() {
                if j > 0 {
                    p = p.add_cached(&row.cached()).point();
                }
                let zi = p.z.invert();
                let (x, y) = (p.x.mul(&zi), p.y.mul(&zi));
                assert_eq!(entry.y_plus_x.bytes(), y.add(&x).carry().bytes(), "row {i}, entry {j}");
                assert_eq!(entry.y_minus_x.bytes(), y.sub(&x).carry().bytes(), "row {i}, entry {j}");
                assert_eq!(entry.xy2d.bytes(), x.mul(&y).mul(&D2).bytes(), "row {i}, entry {j}");
            }
            for _ in 0..8 {
                row = row.double();
            }
        }
    }

    fn scalars() -> std::vec::Vec<[u8; 32]> {
        let mut out = std::vec![[0; 32], [0x88; 32], [0x77; 32], [0xff; 32]];
        for a in &mut out {
            a[31] &= 0x7f;
        }
        let mut seed = 0x2545f4914f6cdd1du64;
        for _ in 0..60 {
            let mut a = [0u8; 32];
            for byte in &mut a {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                *byte = seed as u8;
            }
            a[31] &= 0x7f;
            out.push(a);
        }
        out
    }

    /// The digits lie in [-8, 8] and recompose the scalar, up to 2^255 - 1.
    #[test]
    fn radix_16_digits_recompose_the_scalar() {
        use num_bigint::BigInt;
        for a in scalars() {
            let mut e = [0i8; 64];
            recode(&a, &mut e);
            let mut sum = BigInt::from(0);
            for (i, &d) in e.iter().enumerate() {
                assert!((-8..=8).contains(&d));
                sum += BigInt::from(d) << (4 * i);
            }
            assert_eq!(sum, BigInt::from_bytes_le(num_bigint::Sign::Plus, &a));
        }
    }

    /// The comb agrees with the verification path's table and recoding, a
    /// different route to [s]B, for scalars below L.
    #[test]
    fn the_comb_agrees_with_straus() {
        let zero = PublicScalar::canonical(&[0; 32]).unwrap();
        for a in scalars() {
            let s = PublicScalar::reduce(&{
                let mut wide = [0; 64];
                wide[..32].copy_from_slice(&a);
                wide
            });
            let reduced = s.bytes();
            let comb = base_mul(&reduced);
            let straus = verification_point(&s, &zero, &Point::base());
            let comb = Projective { x: comb.x, y: comb.y, z: comb.z };
            assert_eq!(comb.encode(), straus.encode());
        }
    }

    /// The static table is B's odd multiples, against repeated addition.
    #[test]
    fn base_table_holds_the_odd_multiples_of_b() {
        let base = Point::base();
        let twice = base.double().cached();
        let mut p = base;
        for (i, entry) in B_ODD.iter().enumerate() {
            if i > 0 {
                p = p.add_cached(&twice).point();
            }
            let zi = p.z.invert();
            let (x, y) = (p.x.mul(&zi), p.y.mul(&zi));
            assert_eq!(entry.y_plus_x.bytes(), y.add(&x).carry().bytes(), "entry {i}");
            assert_eq!(entry.y_minus_x.bytes(), y.sub(&x).carry().bytes(), "entry {i}");
            assert_eq!(entry.xy2d.bytes(), x.mul(&y).mul(&D2).bytes(), "entry {i}");
        }
    }
}
