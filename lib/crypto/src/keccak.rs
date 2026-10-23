//! FIPS 202 sponge used only by ML-KEM. Its schedule depends only on the rate
//! and the input and output lengths, which are public; the domain and rate are
//! internal constants. The state is erased when the sponge drops.
use crate::secret::wipe64;
const RC: [u64; 24] = [
    1,
    0x8082,
    0x800000000000808a,
    0x8000000080008000,
    0x808b,
    0x80000001,
    0x8000000080008081,
    0x8000000000008009,
    0x8a,
    0x88,
    0x80008009,
    0x8000000a,
    0x8000808b,
    0x800000000000008b,
    0x8000000000008089,
    0x8000000000008003,
    0x8000000000008002,
    0x8000000000000080,
    0x800a,
    0x800000008000000a,
    0x8000000080008081,
    0x8000000000008080,
    0x80000001,
    0x8000000080008008,
];
/// ρ's rotation of lane x + 5y (FIPS 202 §3.2.2).
const RHO: [u32; 25] = [
    0, 1, 62, 28, 27, 36, 44, 6, 55, 20, 3, 10, 43, 25, 39, 41, 45, 15, 21, 8, 18, 2, 61, 56, 14,
];
pub(crate) struct Sponge {
    state: [u64; 25],
    rate: usize,
    position: usize,
}
impl Drop for Sponge {
    fn drop(&mut self) {
        wipe64(&mut self.state)
    }
}
impl Sponge {
    pub(crate) fn new(rate: usize, domain: u8, parts: &[&[u8]]) -> Self {
        assert!([72, 136, 168].contains(&rate));
        let mut s = Self {
            state: [0; 25],
            rate,
            position: 0,
        };
        for part in parts {
            s.absorb(part)
        }
        s.xor_byte(s.position, domain);
        s.xor_byte(rate - 1, 0x80);
        permute(&mut s.state);
        s.position = 0;
        s
    }
    fn xor_byte(&mut self, at: usize, byte: u8) {
        self.state[at / 8] ^= u64::from(byte) << (8 * (at % 8))
    }
    /// A whole lane at a time where the position is on one; every rate is.
    fn absorb(&mut self, mut data: &[u8]) {
        while !data.is_empty() {
            let at = self.position;
            let step = match data.first_chunk::<8>() {
                Some(word) if at.is_multiple_of(8) => {
                    self.state[at / 8] ^= u64::from_le_bytes(*word);
                    8
                }
                _ => {
                    self.xor_byte(at, data[0]);
                    1
                }
            };
            data = &data[step..];
            self.position += step;
            if self.position == self.rate {
                permute(&mut self.state);
                self.position = 0
            }
        }
    }
    pub(crate) fn read(&mut self, mut output: &mut [u8]) {
        while !output.is_empty() {
            if self.position == self.rate {
                permute(&mut self.state);
                self.position = 0
            }
            let at = self.position;
            let step = if at.is_multiple_of(8) && output.len() >= 8 { 8 } else { 1 };
            let lane = self.state[at / 8].to_le_bytes();
            let (head, rest) = core::mem::take(&mut output).split_at_mut(step);
            head.copy_from_slice(&lane[at % 8..at % 8 + step]);
            output = rest;
            self.position += step
        }
    }
}
/// Keccak-f[1600] (FIPS 202 §3.3), lane x + 5y at index x + 5y. Each loop but
/// the rounds' has constant bounds, for the compiler to unroll.
fn permute(a: &mut [u64; 25]) {
    for &rc in &RC {
        let c: [u64; 5] = core::array::from_fn(|x| a[x] ^ a[x + 5] ^ a[x + 10] ^ a[x + 15] ^ a[x + 20]);
        for x in 0..5 {
            let d = c[(x + 4) % 5] ^ c[(x + 1) % 5].rotate_left(1);
            for y in 0..5 {
                a[x + 5 * y] ^= d
            }
        }
        // ρ and π: lane (x, y), rotated, moves to (y, 2x + 3y).
        let mut b = [0; 25];
        for y in 0..5 {
            for x in 0..5 {
                b[y + 5 * ((2 * x + 3 * y) % 5)] = a[x + 5 * y].rotate_left(RHO[x + 5 * y])
            }
        }
        for y in 0..5 {
            for x in 0..5 {
                a[x + 5 * y] = b[x + 5 * y] ^ (!b[(x + 1) % 5 + 5 * y] & b[(x + 2) % 5 + 5 * y])
            }
        }
        a[0] ^= rc
    }
}
pub(crate) fn h(parts: &[&[u8]]) -> [u8; 32] {
    let mut out = [0; 32];
    Sponge::new(136, 6, parts).read(&mut out);
    out
}
pub(crate) fn g(parts: &[&[u8]]) -> [u8; 64] {
    let mut out = [0; 64];
    Sponge::new(72, 6, parts).read(&mut out);
    out
}
pub(crate) fn j(parts: &[&[u8]]) -> [u8; 32] {
    let mut out = [0; 32];
    Sponge::new(136, 31, parts).read(&mut out);
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fips202_empty() {
        assert_eq!(
            h(&[]),
            [
                0xa7, 0xff, 0xc6, 0xf8, 0xbf, 0x1e, 0xd7, 0x66, 0x51, 0xc1, 0x47, 0x56, 0xa0, 0x61,
                0xd6, 0x62, 0xf5, 0x80, 0xff, 0x4d, 0xe4, 0x3b, 0x49, 0xfa, 0x82, 0xd8, 0x0a, 0x4b,
                0x80, 0xf8, 0x43, 0x4a
            ]
        );
    }

    /// The lane-wise and byte-wise paths agree: any split of the input into
    /// parts, and of the output into reads, gives the one-piece answer.
    #[test]
    fn splits_do_not_change_the_output() {
        let data: [u8; 400] = core::array::from_fn(|i| (i * 7 + 3) as u8);
        for rate in [72, 136, 168] {
            let mut want = [0; 400];
            Sponge::new(rate, 31, &[&data]).read(&mut want);
            for cut in [1, 3, 8, 9, 15, 64, 71, 135, 167, 169, 399] {
                let (a, b) = data.split_at(cut);
                let mut got = [0; 400];
                let mut sponge = Sponge::new(rate, 31, &[a, b]);
                let (head, tail) = got.split_at_mut(cut);
                for piece in head.chunks_mut(5) {
                    sponge.read(piece)
                }
                sponge.read(tail);
                assert_eq!(got, want, "rate {rate}, cut {cut}");
            }
        }
    }
}
