//! SipHash-2-4 (Aumasson and Bernstein, "SipHash: a fast short-input PRF"): the keyed function
//! behind every value a net crate keeps unpredictable, each purpose with its own key.

pub type Key = [u8; 16];

fn round(v: &mut [u64; 4]) {
    v[0] = v[0].wrapping_add(v[1]);
    v[1] = v[1].rotate_left(13) ^ v[0];
    v[0] = v[0].rotate_left(32);
    v[2] = v[2].wrapping_add(v[3]);
    v[3] = v[3].rotate_left(16) ^ v[2];
    v[0] = v[0].wrapping_add(v[3]);
    v[3] = v[3].rotate_left(21) ^ v[0];
    v[2] = v[2].wrapping_add(v[1]);
    v[1] = v[1].rotate_left(17) ^ v[2];
    v[2] = v[2].rotate_left(32);
}

fn compress(v: &mut [u64; 4], m: u64) {
    v[3] ^= m;
    round(v);
    round(v);
    v[0] ^= m;
}

pub fn siphash24(key: &Key, data: &[u8]) -> u64 {
    let (k0, k1) = key.split_at(8);
    let k0 = k0.iter().rev().fold(0u64, |a, &b| a.rotate_left(8) | u64::from(b));
    let k1 = k1.iter().rev().fold(0u64, |a, &b| a.rotate_left(8) | u64::from(b));
    let mut v = [k0 ^ 0x736f_6d65_7073_6575, k1 ^ 0x646f_7261_6e64_6f6d, k0 ^ 0x6c79_6765_6e65_7261, k1 ^ 0x7465_6462_7974_6573];
    let (words, tail) = data.as_chunks::<8>();
    for word in words {
        compress(&mut v, u64::from_le_bytes(*word));
    }
    let length = u64::from(data.len().to_le_bytes()[0]).rotate_right(8);
    compress(&mut v, tail.iter().rev().fold(0u64, |a, &b| a.rotate_left(8) | u64::from(b)) | length);
    v[2] ^= 0xff;
    for _ in 0..4 {
        round(&mut v);
    }
    v[0] ^ v[1] ^ v[2] ^ v[3]
}

pub fn low32(key: &Key, data: &[u8]) -> u32 {
    let [a, b, c, d, ..] = siphash24(key, data).to_le_bytes();
    u32::from_le_bytes([a, b, c, d])
}
