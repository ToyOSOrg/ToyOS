//! FIPS 202 sponge used only by ML-KEM. Absorption and permutation have fixed
//! schedules for public lengths. Rate/domain separation are internal constants.
//! State and explicit permutation workspaces are erased, including on failure.
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
const ROT: [u32; 25] = [
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
        for p in parts {
            for &b in *p {
                s.state[s.position / 8] ^= u64::from(b) << (8 * (s.position % 8));
                s.position += 1;
                if s.position == rate {
                    s.permute();
                    s.position = 0
                }
            }
        }
        s.state[s.position / 8] ^= u64::from(domain) << (8 * (s.position % 8));
        let last = rate - 1;
        s.state[last / 8] ^= 0x80u64 << (8 * (last % 8));
        s.permute();
        s.position = 0;
        s
    }
    fn permute(&mut self) {
        let mut c = [0; 5];
        let mut d = [0; 5];
        let mut b = [0; 25];
        for rc in RC {
            for x in 0..5 {
                c[x] = (0..5).fold(0, |c, y| c ^ self.state[x + 5 * y])
            }
            for x in 0..5 {
                d[x] = c[(x + 4) % 5] ^ c[(x + 1) % 5].rotate_left(1)
            }
            for y in 0..5 {
                for x in 0..5 {
                    let i = x + 5 * y;
                    b[y + 5 * ((2 * x + 3 * y) % 5)] = (self.state[i] ^ d[x]).rotate_left(ROT[i]);
                }
            }
            for y in 0..5 {
                for x in 0..5 {
                    self.state[x + 5 * y] =
                        b[x + 5 * y] ^ (!b[(x + 1) % 5 + 5 * y] & b[(x + 2) % 5 + 5 * y]);
                }
            }
            self.state[0] ^= rc;
        }
        wipe64(&mut c);
        wipe64(&mut d);
        wipe64(&mut b)
    }
    pub(crate) fn read(&mut self, output: &mut [u8]) {
        for byte in output {
            if self.position == self.rate {
                self.permute();
                self.position = 0
            }
            *byte = (self.state[self.position / 8] >> (8 * (self.position % 8))) as u8;
            self.position += 1
        }
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
}
