//! Arithmetic modulo p = 2^255 - 19. A [`Tight`] element is what a
//! multiplication or a carry leaves, a [`Loose`] one the sum or difference of
//! two tight ones. Multiplication takes either; addition and subtraction take
//! tight operands. Every operation has a fixed schedule and no branch or
//! address that depends on an element. Encodings and equality are public at
//! verification boundaries.
//!
//! The representation is the target's: four 64-bit words on x86-64, where
//! multiplication takes MULX, ADCX and ADOX when the CPU has them, and five
//! 51-bit limbs elsewhere.
//!
//! Elements are `Copy` values and are not erased: an owner of secret state
//! erases its own storage (see `secret`).

#[cfg(target_arch = "x86_64")]
mod radix64;
#[cfg(target_arch = "x86_64")]
pub(crate) use radix64::{Loose, Multiply, Tight};

#[cfg(not(target_arch = "x86_64"))]
mod radix51;
#[cfg(not(target_arch = "x86_64"))]
pub(crate) use radix51::{Loose, Multiply, Tight};

impl Tight {
    /// self^(2^k), for k >= 1.
    fn square_times(&self, k: u32) -> Tight {
        debug_assert!(k >= 1);
        let mut r = self.square();
        for _ in 1..k {
            r = r.square();
        }
        r
    }

    /// self^(2^250 - 1) and self^11, the shared head of the inversion and
    /// square-root chains (Bernstein 2006, section 5).
    fn pow_2_250_1(&self) -> (Tight, Tight) {
        let z2 = self.square();
        let z9 = z2.square_times(2).mul(self);
        let z11 = z9.mul(&z2);
        let z_5_0 = z11.square().mul(&z9);
        let z_10_0 = z_5_0.square_times(5).mul(&z_5_0);
        let z_20_0 = z_10_0.square_times(10).mul(&z_10_0);
        let z_40_0 = z_20_0.square_times(20).mul(&z_20_0);
        let z_50_0 = z_40_0.square_times(10).mul(&z_10_0);
        let z_100_0 = z_50_0.square_times(50).mul(&z_50_0);
        let z_200_0 = z_100_0.square_times(100).mul(&z_100_0);
        let z_250_0 = z_200_0.square_times(50).mul(&z_50_0);
        (z_250_0, z11)
    }

    /// self^(p - 2) = self^(2^255 - 21): 254 squarings and 11 multiplications.
    pub(crate) fn invert(&self) -> Tight {
        let (z_250_0, z11) = self.pow_2_250_1();
        z_250_0.square_times(5).mul(&z11)
    }

    /// self^((p - 5) / 8) = self^(2^252 - 3), for square roots.
    pub(crate) fn pow_p58(&self) -> Tight {
        let (z_250_0, _) = self.pow_2_250_1();
        z_250_0.square_times(2).mul(self)
    }

    /// [`Tight::invert`] for tables built at compile time: square and
    /// multiply over the exponent's bits, 2^255 - 21 having every bit below
    /// 255 set but bits 2 and 4.
    pub(crate) const fn inverted(&self) -> Tight {
        let mut r = Tight::ONE;
        let mut i = 255;
        while i > 0 {
            i -= 1;
            r = r.squared();
            if i != 2 && i != 4 {
                r = r.times(self);
            }
        }
        r
    }

    /// One when the canonical encoding's low bit is set.
    pub(crate) fn is_negative(&self) -> u64 {
        u64::from(self.bytes()[0] & 1)
    }

    /// One when both encode the same element.
    pub(crate) fn eq(&self, b: &Self) -> u64 {
        u64::from(crate::ct::equal(&self.bytes(), &b.bytes()))
    }

    pub(crate) fn is_zero(&self) -> u64 {
        self.eq(&Self::ZERO)
    }
}
