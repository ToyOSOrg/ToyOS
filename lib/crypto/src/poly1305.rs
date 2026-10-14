//! RFC 8439 Poly1305. Five 26-bit limbs and fixed carry passes; no key-indexed
//! table or content-dependent branch. A complete block adds 2^128 before
//! multiplication. The largest multiply accumulation is below 2^61.

use crate::secret::{Secret, wipe, wipe64};

const MASK: u64 = (1 << 26) - 1;

pub(crate) struct Poly1305 {
    r: [u64; 5],
    h: [u64; 5],
    pad: Secret<16>,
    buffer: [u8; 16],
    filled: usize,
}

impl Poly1305 {
    pub(crate) fn new(key: &[u8; 32]) -> Self {
        let mut r = Secret([0; 16]);
        r.0.copy_from_slice(&key[..16]);
        for i in [3, 7, 11, 15] {
            r.0[i] &= 15;
        }
        for i in [4, 8, 12] {
            r.0[i] &= 252;
        }
        let n = u128::from_le_bytes(r.0);
        let mut pad = Secret([0; 16]);
        pad.0.copy_from_slice(&key[16..]);
        Self {
            r: core::array::from_fn(|i| ((n >> (26 * i)) as u64) & MASK),
            h: [0; 5],
            pad,
            buffer: [0; 16],
            filled: 0,
        }
    }
    fn absorb(&mut self, block: &[u8; 16], full: bool) {
        let n = u128::from_le_bytes(*block);
        for i in 0..5 {
            self.h[i] += ((n >> (26 * i)) as u64) & MASK;
        }
        self.h[4] += u64::from(full) << 24;
        let mut d = [0u64; 5];
        for (i, out) in d.iter_mut().enumerate() {
            for j in 0..5 {
                let k = (i + 5 - j) % 5;
                let factor = if j > i { 5 } else { 1 };
                *out += self.h[j] * self.r[k] * factor;
            }
        }
        for i in 0..4 {
            d[i + 1] += d[i] >> 26;
            self.h[i] = d[i] & MASK;
        }
        self.h[4] = d[4] & MASK;
        self.h[0] += (d[4] >> 26) * 5;
        self.h[1] += self.h[0] >> 26;
        self.h[0] &= MASK;
        wipe64(&mut d);
    }
    pub(crate) fn update(&mut self, mut data: &[u8]) {
        if self.filled != 0 {
            let n = data.len().min(16 - self.filled);
            self.buffer[self.filled..self.filled + n].copy_from_slice(&data[..n]);
            self.filled += n;
            data = &data[n..];
            if self.filled == 16 {
                let mut b = self.buffer;
                self.absorb(&b, true);
                wipe(&mut b);
                self.filled = 0;
            }
        }
        while data.len() >= 16 {
            self.absorb(data[..16].try_into().unwrap(), true);
            data = &data[16..];
        }
        if !data.is_empty() {
            self.buffer[self.filled..self.filled + data.len()].copy_from_slice(data);
            self.filled += data.len();
        }
    }
    pub(crate) fn pad16(&mut self) {
        if self.filled != 0 {
            let count = 16 - self.filled;
            self.update(&[0; 16][..count]);
        }
    }
    pub(crate) fn finish(mut self) -> [u8; 16] {
        if self.filled != 0 {
            self.buffer[self.filled] = 1;
            self.buffer[self.filled + 1..].fill(0);
            let mut b = self.buffer;
            self.absorb(&b, false);
            wipe(&mut b);
        }
        for _ in 0..2 {
            for i in 0..4 {
                self.h[i + 1] += self.h[i] >> 26;
                self.h[i] &= MASK;
            }
            self.h[0] += (self.h[4] >> 26) * 5;
            self.h[4] &= MASK;
        }
        let mut g = [0u64; 5];
        g[0] = self.h[0] + 5;
        for i in 0..4 {
            g[i + 1] = self.h[i + 1] + (g[i] >> 26);
            g[i] &= MASK;
        }
        g[4] = g[4].wrapping_sub(1 << 26);
        let reduce = 1 ^ (g[4] >> 63);
        g[4] &= MASK;
        for (h, &g) in self.h.iter_mut().zip(&g) {
            *h = crate::ct::select(*h, g, reduce);
        }
        let n = self
            .h
            .iter()
            .enumerate()
            .fold(0u128, |n, (i, &h)| n | ((h as u128) << (26 * i)));
        let tag = n
            .wrapping_add(u128::from_le_bytes(self.pad.0))
            .to_le_bytes();
        wipe64(&mut g);
        tag
    }
}
impl Drop for Poly1305 {
    fn drop(&mut self) {
        wipe64(&mut self.r);
        wipe64(&mut self.h);
        wipe(&mut self.buffer);
    }
}
