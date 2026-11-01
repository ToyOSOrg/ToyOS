//! scrypt (RFC 7914), for keys derived from passphrases.
//!
//! Untrusted cost parameters become a [`Params`] only within RFC 7914 §2's
//! bounds, [`MAX_MEMORY`] and the caller's [`Limits`], so a workspace sized
//! from a [`Params`] is within the caller's budget and its derivation within
//! the caller's work. Parameters, lengths and the workspace's size are public.
//!
//! PBKDF2 and Salsa20/8 do not let contents choose branches or addresses.
//! ROMix does, by design: its second loop reads the table entry the secret
//! state names (RFC 7914 §5), so its memory access pattern depends on the
//! passphrase. ROMix is exempt from the constant-time measurement.
//!
//! The workspace is erased before [`derive`] returns.

use core::num::NonZeroU32;

use crate::{
    Error,
    hash::Sha256,
    kdf::{Hmac, pbkdf2_blocks},
    secret::{Secret, Workspace, wipe32},
};

/// The most ROMix's table, 128 * r * N bytes, may occupy under any caller's
/// limit: 1 GiB, which RFC 7914 §12's largest vector fills.
pub const MAX_MEMORY: usize = 1 << 30;

/// What a caller will spend on one derivation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    /// Bytes of workspace, 128 * r * (N + 2).
    pub memory: usize,
    /// N * r * p, to which the derivation's time is proportional.
    pub work: u64,
}

/// scrypt's cost: N = 2^`log_n`, block size r and parallelization p.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Params {
    log_n: u8,
    r: u32,
    p: u32,
}

impl Params {
    /// Refuses all but RFC 7914 §2's parameters, N > 1, N < 2^(16 r), r and
    /// p positive, p * 128 * r <= (2^32 - 1) * 32, and a table past
    /// [`MAX_MEMORY`] or a cost past `limits`.
    pub fn new(log_n: u8, r: u32, p: u32, limits: Limits) -> Result<Self, Error> {
        // N < 2^32 keeps every product below in range; `MAX_MEMORY` alone
        // holds N to 2^23.
        if !(1..32).contains(&log_n) || r == 0 || p == 0 {
            return Err(Error::InvalidParameters);
        }
        let (n, r128, p128) = (1u128 << log_n, u128::from(r), u128::from(p));
        let valid = u128::from(log_n) < 16 * r128
            && p128 * 128 * r128 <= u128::from(u32::MAX) * 32
            && 128 * r128 * n <= MAX_MEMORY as u128
            && 128 * r128 * (n + 2) <= limits.memory as u128
            && n * r128 * p128 <= u128::from(limits.work);
        if valid { Ok(Self { log_n, r, p }) } else { Err(Error::InvalidParameters) }
    }

    /// The words [`derive`]'s workspace holds: the table's N entries and two
    /// more, each 32 * r words.
    pub fn workspace_len(&self) -> usize {
        self.entry() * ((1 << self.log_n) + 2)
    }

    fn entry(&self) -> usize {
        32 * self.r as usize
    }
}

/// scrypt of `passphrase` and `salt` into `output`, which is 1 to
/// (2^32 - 1) * 32 bytes. `workspace` is exactly
/// [`Params::workspace_len`] words.
pub fn derive(passphrase: &[u8], salt: &[u8], params: Params, workspace: &mut [u32], output: &mut [u8]) -> Result<(), Error> {
    let _protection = crate::timing::protect();
    let workspace = Erased(workspace);
    if workspace.0.len() != params.workspace_len() || output.is_empty() || output.len().div_ceil(32) > u32::MAX as usize {
        return Err(Error::Length);
    }
    let words = params.entry();
    let (table, rest) = workspace.0.split_at_mut(words << params.log_n);
    let (x, y) = rest.split_at_mut(words);
    let mut scratch = Workspace([0u32; 16]);
    let mut bytes = Secret([0u8; 64]);

    let keyed = Hmac::<Sha256>::new(passphrase)?;
    let mut salted = keyed.fork();
    salted.update(salt)?;
    let mut mixed = keyed.fork();
    for lane in 0..params.p {
        // Each lane is 4r of PBKDF2's 32-byte blocks; `Params::new` bounds
        // every lane's index below 2^32.
        let first = lane * 4 * params.r + 1;
        for (k, piece) in table.as_chunks_mut::<8>().0[..4 * params.r as usize].iter_mut().enumerate() {
            pbkdf2_blocks(&keyed, &salted, first + k as u32, NonZeroU32::MIN, &mut bytes.0[..32])?;
            for (word, b) in piece.iter_mut().zip(bytes.0.as_chunks::<4>().0) {
                *word = u32::from_le_bytes(*b);
            }
        }
        ro_mix(table, x, y, &mut scratch.0);
        for block in x.as_chunks::<16>().0 {
            for (b, word) in bytes.0.as_chunks_mut::<4>().0.iter_mut().zip(block) {
                *b = word.to_le_bytes();
            }
            mixed.update(&bytes.0)?;
        }
    }
    pbkdf2_blocks(&keyed, &mixed, 1, NonZeroU32::MIN, output)
}

/// The caller's workspace, erased however [`derive`] returns.
struct Erased<'a>(&'a mut [u32]);

impl Drop for Erased<'_> {
    fn drop(&mut self) {
        wipe32(self.0);
    }
}

/// RFC 7914 §5's ROMix of the table's first entry, its result left in `x`.
/// `y` is an entry's room for the second loop to alternate with.
fn ro_mix(table: &mut [u32], x: &mut [u32], y: &mut [u32], scratch: &mut [u32; 16]) {
    let words = x.len();
    let n = table.len() / words;
    for i in 1..n {
        let (done, next) = table.split_at_mut(i * words);
        block_mix(&done[(i - 1) * words..], &mut next[..words], scratch);
    }
    block_mix(&table[(n - 1) * words..], x, scratch);
    // N is even, so the result lands back in `x`.
    for _ in 0..n / 2 {
        mix_with_entry(table, x, y, scratch);
        mix_with_entry(table, y, x, scratch);
    }
}

/// `to` = BlockMix(`from` xor the entry Integerify(`from`) mod N names).
/// That index is secret: the memory access ROMix exists to make.
fn mix_with_entry(table: &[u32], from: &mut [u32], to: &mut [u32], scratch: &mut [u32; 16]) {
    let words = from.len();
    // Integerify's low word: N <= 2^23 under `MAX_MEMORY`.
    let j = from[words - 16] as usize & (table.len() / words - 1);
    for (a, b) in from.iter_mut().zip(&table[j * words..(j + 1) * words]) {
        *a ^= b;
    }
    block_mix(from, to, scratch);
}

/// RFC 7914 §4's BlockMix of `input` into `output`, each 2r blocks of 16
/// words: the even blocks' results first, then the odd ones'.
fn block_mix(input: &[u32], output: &mut [u32], x: &mut [u32; 16]) {
    let r = input.len() / 32;
    x.copy_from_slice(&input[input.len() - 16..]);
    for (i, block) in input.as_chunks::<16>().0.iter().enumerate() {
        for (x, b) in x.iter_mut().zip(block) {
            *x ^= b;
        }
        salsa20_8(x);
        let at = (i / 2 + (i % 2) * r) * 16;
        output[at..at + 16].copy_from_slice(x);
    }
}

/// RFC 7914 §3's Salsa20/8 core, in place over 16 words read from bytes
/// little-endian.
#[inline]
pub fn salsa20_8(block: &mut [u32; 16]) {
    let mut x = *block;
    for _ in 0..4 {
        quarter(&mut x, 0, 4, 8, 12);
        quarter(&mut x, 5, 9, 13, 1);
        quarter(&mut x, 10, 14, 2, 6);
        quarter(&mut x, 15, 3, 7, 11);
        quarter(&mut x, 0, 1, 2, 3);
        quarter(&mut x, 5, 6, 7, 4);
        quarter(&mut x, 10, 11, 8, 9);
        quarter(&mut x, 15, 12, 13, 14);
    }
    for (b, x) in block.iter_mut().zip(x) {
        *b = b.wrapping_add(x);
    }
}

#[inline(always)]
fn quarter(x: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    x[b] ^= x[a].wrapping_add(x[d]).rotate_left(7);
    x[c] ^= x[b].wrapping_add(x[a]).rotate_left(9);
    x[d] ^= x[c].wrapping_add(x[b]).rotate_left(13);
    x[a] ^= x[d].wrapping_add(x[c]).rotate_left(18);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words<const N: usize>(hex: &str) -> [u32; N] {
        let hex: std::string::String = hex.split_whitespace().collect();
        assert_eq!(hex.len(), 8 * N);
        core::array::from_fn(|i| {
            let byte = |k: usize| u32::from(u8::from_str_radix(&hex[8 * i + 2 * k..][..2], 16).unwrap());
            byte(0) | byte(1) << 8 | byte(2) << 16 | byte(3) << 24
        })
    }

    const BLOCK_MIX_IN: &str = "f7 ce 0b 65 3d 2d 72 a4 10 8c f5 ab e9 12 ff dd 77 76 16 db bb 27 a7 0e 82 04 f3 ae 2d 0f 6f ad
        89 f6 8f 48 11 d1 e8 7b cc 3b d7 40 0a 9f fd 29 09 4f 01 84 63 95 74 f3 9a e5 a1 31 52 17 bc d7
        89 49 91 44 72 13 bb 22 6c 25 b5 4d a8 63 70 fb cd 98 43 80 37 46 66 bb 8f fc b5 bf 40 c2 54 b0
        67 d2 7c 51 ce 4a d5 fe d8 29 c9 0b 50 5a 57 1b 7f 4d 1c ad 6a 52 3c da 77 0e 67 bc ea af 7e 89";

    /// RFC 7914 §8.
    #[test]
    fn rfc7914_salsa20_8_core() {
        let mut block = words::<16>(
            "7e 87 9a 21 4f 3e c9 86 7c a9 40 e6 41 71 8f 26 ba ee 55 5b 8c 61 c1 b5 0d f8 46 11 6d cd 3b 1d
             ee 24 f3 19 df 9b 3d 85 14 12 1e 4b 5a c5 aa 32 76 02 1d 29 09 c7 48 29 ed eb c6 8d b8 b8 c2 5e",
        );
        salsa20_8(&mut block);
        assert_eq!(
            block,
            words(
                "a4 1f 85 9c 66 08 cc 99 3b 81 ca cb 02 0c ef 05 04 4b 21 81 a2 fd 33 7d fd 7b 1c 63 96 68 2f 29
                 b4 39 31 68 e3 c9 e6 bc fe 6b c5 b7 a0 6d 96 ba e4 24 cc 10 2c 91 74 5c 24 ad 67 3d c7 61 8f 81"
            )
        );
    }

    /// RFC 7914 §9, r = 1.
    #[test]
    fn rfc7914_block_mix() {
        let mut out = [0; 32];
        block_mix(&words::<32>(BLOCK_MIX_IN), &mut out, &mut [0; 16]);
        assert_eq!(
            out,
            words(
                "a4 1f 85 9c 66 08 cc 99 3b 81 ca cb 02 0c ef 05 04 4b 21 81 a2 fd 33 7d fd 7b 1c 63 96 68 2f 29
                 b4 39 31 68 e3 c9 e6 bc fe 6b c5 b7 a0 6d 96 ba e4 24 cc 10 2c 91 74 5c 24 ad 67 3d c7 61 8f 81
                 20 ed c9 75 32 38 81 a8 05 40 f6 4c 16 2d cd 3c 21 07 7c fe 5f 8d 5f e2 b1 a4 16 8f 95 36 78 b7
                 7d 3b 3d 80 3b 60 e4 ab 92 09 96 e5 9b 4d 53 b6 5d 2a 22 58 77 d5 ed f5 84 2c b9 f1 4e ef e4 25"
            )
        );
    }

    /// RFC 7914 §10, r = 1 and N = 16.
    #[test]
    fn rfc7914_ro_mix() {
        let mut table = [0; 16 * 32];
        table[..32].copy_from_slice(&words::<32>(BLOCK_MIX_IN));
        let (mut x, mut y) = ([0; 32], [0; 32]);
        ro_mix(&mut table, &mut x, &mut y, &mut [0; 16]);
        assert_eq!(
            x,
            words(
                "79 cc c1 93 62 9d eb ca 04 7f 0b 70 60 4b f6 b6 2c e3 dd 4a 96 26 e3 55 fa fc 61 98 e6 ea 2b 46
                 d5 84 13 67 3b 99 b0 29 d6 65 c3 57 60 1f b4 26 a0 b2 f4 bb a2 00 ee 9f 0a 43 d1 9b 57 1a 9c 71
                 ef 11 42 e6 5d 5a 26 6f dd ca 83 2c e5 9f aa 7c ac 0b 9c f1 be 2b ff ca 30 0d 01 ee 38 76 19 c4
                 ae 12 fd 44 38 f2 03 a0 e4 e1 c4 7e c3 14 86 1f 4e 90 87 cb 33 39 6a 68 73 e8 f9 d2 53 9a 4b 8e"
            )
        );
    }

    const ANY: Limits = Limits { memory: usize::MAX, work: u64::MAX };

    #[test]
    fn parameters_past_the_rfc_or_max_memory_are_refused() {
        let refused = Err(Error::InvalidParameters);
        assert_eq!(Params::new(0, 1, 1, ANY), refused, "N = 1");
        assert_eq!(Params::new(1, 0, 1, ANY), refused, "r = 0");
        assert_eq!(Params::new(1, 1, 0, ANY), refused, "p = 0");
        assert!(Params::new(15, 1, 1, ANY).is_ok());
        assert_eq!(Params::new(16, 1, 1, ANY), refused, "N = 2^(16 r)");
        assert!(Params::new(20, 8, 1, ANY).is_ok(), "a table of exactly MAX_MEMORY");
        assert_eq!(Params::new(21, 8, 1, ANY), refused, "a table past MAX_MEMORY");
        assert!(Params::new(1, 1 << 22, 1, ANY).is_ok(), "two entries of exactly MAX_MEMORY");
        assert_eq!(Params::new(1, (1 << 22) + 1, 1, ANY), refused, "two entries past MAX_MEMORY");
        assert_eq!(Params::new(255, u32::MAX, 1, ANY), refused);
        let most = u32::MAX / 4;
        assert!(Params::new(1, 1, most, ANY).is_ok(), "p * 128 * r = (2^32 - 1) * 32 less 96");
        assert_eq!(Params::new(1, 1, most + 1, ANY), refused, "p * 128 * r past (2^32 - 1) * 32");
        assert_eq!(Params::new(1, 1, u32::MAX, ANY), refused);
    }

    /// At bcachefs's default cost, N = 2^14, r = 8 and p = 16: a workspace
    /// of 128 * 8 * (2^14 + 2) bytes and work 2^21.
    #[test]
    fn a_cost_past_the_callers_limits_is_refused() {
        let (memory, work) = (128 * 8 * ((1 << 14) + 2), 1 << 21);
        let refused = Err(Error::InvalidParameters);
        let params = Params::new(14, 8, 16, Limits { memory, work }).unwrap();
        assert_eq!(params.workspace_len() * 4, memory);
        assert_eq!(Params::new(14, 8, 16, Limits { memory: memory - 1, work }), refused, "a byte past the memory limit");
        assert_eq!(Params::new(14, 8, 16, Limits { memory, work: work - 1 }), refused, "past the work limit");
        assert_eq!(Params::new(14, 8, 17, Limits { memory, work }), refused, "one lane more");
    }

    #[test]
    fn a_workspace_of_any_other_length_and_an_empty_output_are_refused() {
        let params = Params::new(1, 1, 1, ANY).unwrap();
        assert_eq!(params.workspace_len(), 4 * 32);
        let mut out = [0; 32];
        for len in [0, 4 * 32 - 1, 4 * 32 + 1] {
            assert_eq!(derive(b"", b"", params, &mut std::vec![0; len], &mut out), Err(Error::Length));
        }
        assert_eq!(derive(b"", b"", params, &mut [0; 4 * 32], &mut []), Err(Error::Length));
    }

    #[test]
    fn a_refused_derivation_erases_the_workspace() {
        let params = Params::new(1, 1, 1, ANY).unwrap();
        let mut workspace = [0xdead_beef; 4 * 32];
        assert_eq!(derive(b"", b"", params, &mut workspace, &mut []), Err(Error::Length));
        assert!(workspace.iter().all(|&w| w == 0));
    }

    #[test]
    fn the_workspace_is_erased() {
        let params = Params::new(2, 2, 2, ANY).unwrap();
        let mut workspace = std::vec![u32::MAX; params.workspace_len()];
        derive(b"passphrase", b"salt", params, &mut workspace, &mut [0; 32]).unwrap();
        assert!(workspace.iter().all(|&w| w == 0));
    }
}
