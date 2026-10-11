//! The ChaCha20 block function, as RFC 8439 §2.3 states it.

use crate::wipe;

/// "expand 32-byte k", the constant of §2.3's first four words.
const CONSTANT: [u32; 4] = [0x6170_7865, 0x3320_646e, 0x7962_2d32, 0x6b20_6574];

/// §2.1's quarter round, on four words of the state.
#[inline(always)]
pub(crate) fn quarter_round(s: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(16);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(12);
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(8);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(7);
}

/// The 64-byte block `key`, `counter` and `nonce` give, into `out`. Both
/// states this works in hold the key, and are wiped before it returns.
pub(crate) fn block(key: &[u8; 32], counter: u32, nonce: &[u8; 12], out: &mut [u8; 64]) {
    let mut initial = [0u32; 16];
    initial[..4].copy_from_slice(&CONSTANT);
    for (at, bytes) in key.as_chunks::<4>().0.iter().enumerate() {
        initial[4 + at] = u32::from_le_bytes(*bytes);
    }
    initial[12] = counter;
    for (at, bytes) in nonce.as_chunks::<4>().0.iter().enumerate() {
        initial[13 + at] = u32::from_le_bytes(*bytes);
    }

    let mut state = initial;
    for _ in 0..10 {
        quarter_round(&mut state, 0, 4, 8, 12);
        quarter_round(&mut state, 1, 5, 9, 13);
        quarter_round(&mut state, 2, 6, 10, 14);
        quarter_round(&mut state, 3, 7, 11, 15);
        quarter_round(&mut state, 0, 5, 10, 15);
        quarter_round(&mut state, 1, 6, 11, 12);
        quarter_round(&mut state, 2, 7, 8, 13);
        quarter_round(&mut state, 3, 4, 9, 14);
    }
    for (at, bytes) in out.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        *bytes = state[at].wrapping_add(initial[at]).to_le_bytes();
    }
    wipe(&mut initial);
    wipe(&mut state);
}
