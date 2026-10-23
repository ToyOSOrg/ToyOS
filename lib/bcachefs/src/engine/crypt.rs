//! bcachefs's encryption: raw ChaCha20 at a chosen block counter, and
//! Poly1305 keyed from a separate nonce domain over the ciphertext alone.
//! This is not RFC 8439's AEAD.

use alloc::vec;
use toyos_crypto::{chacha, poly1305, scrypt};

use super::superblock::{Crypt, Kdf};
use super::{Error, Result};

/// The magic a correct unwrap yields, "bch**key".
const KEY_MAGIC: [u8; 8] = *b"bch**key";
/// scrypt's salt, which the format fixes.
const SALT: &[u8] = b"bcache\0";

/// The smallest scrypt parameters the format admits: N = 2,
/// r = p = 1. For a high-entropy passphrase scrypt adds nothing.
pub const MIN_KDF: Kdf = Kdf { log_n: 1, log_r: 0, log_p: 0 };

/// Each structure's domain, XORed into its nonce's last word.
#[derive(Clone, Copy)]
pub(crate) enum Domain {
    Extent = 0x1000_0000,
    Node = 0x2000_0000,
    Journal = 0x3000_0000,
}
/// Selects the MAC key's nonce.
const MAC: u32 = 0x8000_0000;

/// A nonce: `d0` is ChaCha20's first block, `d1`–`d3` its 96-bit nonce.
#[derive(Clone, Copy)]
pub(crate) struct Nonce([u32; 4]);

impl Nonce {
    /// The record at byte `offset` of node `seq`, whose bset is
    /// stamped `journal_seq`.
    pub(crate) fn node(offset: u32, seq: u64, journal_seq: u64) -> Nonce {
        Nonce([offset, seq as u32, (seq >> 32) as u32, journal_seq as u32 ^ Domain::Node as u32])
    }

    /// The journal entry of sequence `seq`.
    pub(crate) fn journal(seq: u64) -> Nonce {
        Nonce([0, seq as u32, (seq >> 32) as u32, Domain::Journal as u32])
    }

    /// An uncompressed extent of version `(hi, lo)` whose crc
    /// nonce field is `crc_nonce`.
    pub(crate) fn extent(hi: u32, lo: u64, crc_nonce: u32) -> Nonce {
        Nonce([8 * crc_nonce, lo as u32, (lo >> 32) as u32, hi ^ Domain::Extent as u32])
    }

    /// This nonce `blocks` ChaCha20 blocks on.
    pub(crate) fn plus(self, blocks: u32) -> Nonce {
        let [d0, d1, d2, d3] = self.0;
        Nonce([d0 + blocks, d1, d2, d3])
    }

    fn parts(self) -> (u32, [u8; 12]) {
        let [d0, d1, d2, d3] = self.0;
        let mut n = [0u8; 12];
        n[..4].copy_from_slice(&d1.to_le_bytes());
        n[4..8].copy_from_slice(&d2.to_le_bytes());
        n[8..].copy_from_slice(&d3.to_le_bytes());
        (d0, n)
    }
}

/// A volume's master key, decrypted. It never reaches the medium unwrapped.
pub(crate) struct MasterKey([u8; 32]);

impl Drop for MasterKey {
    fn drop(&mut self) {
        erase(&mut self.0);
    }
}

/// Overwrites a secret before it goes out of scope. `black_box` makes the
/// compiler treat the zeros as read, so the stores are not elided.
fn erase<const N: usize>(secret: &mut [u8; N]) {
    *secret = [0; N];
    core::hint::black_box(secret);
}

impl MasterKey {
    pub(crate) fn new(key: [u8; 32]) -> Self {
        MasterKey(key)
    }

    /// `data` XORed with the keystream from `nonce`'s first block.
    pub(crate) fn xor(&self, nonce: Nonce, data: &mut [u8]) -> Result<()> {
        let (counter, n) = nonce.parts();
        chacha::xor(&self.0, &n, counter, data).map_err(|_| Error::Corrupt("keystream past its counter"))
    }

    fn mac_key(&self, nonce: Nonce) -> Result<[u8; 32]> {
        let [d0, d1, d2, d3] = nonce.0;
        let mut key = [0u8; 32];
        self.xor(Nonce([d0, d1, d2, d3 ^ MAC]), &mut key)?;
        Ok(key)
    }

    /// The 128-bit MAC of `data` under `nonce`.
    pub(crate) fn mac(&self, nonce: Nonce, data: &[u8]) -> Result<[u8; 16]> {
        let mut key = self.mac_key(nonce)?;
        let tag = poly1305::tag(&key, data);
        erase(&mut key);
        Ok(tag)
    }

    /// `Ok` when `tag` is the first `N` bytes of `data`'s MAC under `nonce`:
    /// 10 or 16, as the checksum type fixes.
    pub(crate) fn verify<const N: usize>(&self, nonce: Nonce, data: &[u8], tag: &[u8; N], what: &'static str) -> Result<()> {
        let mut key = self.mac_key(nonce)?;
        let verdict = poly1305::verify_truncated(&key, data, tag).map_err(|_| Error::Checksum(what));
        erase(&mut key);
        verdict
    }

    /// The key of a volume formatted without a passphrase, which
    /// `crypt` holds in clear behind the magic.
    pub(crate) fn stored(crypt: &Crypt) -> Option<MasterKey> {
        (crypt.wrapped[..8] == KEY_MAGIC).then(|| MasterKey(crypt.wrapped[8..].try_into().unwrap()))
    }

    /// `crypt`'s key unwrapped under `passphrase`, its scrypt within
    /// `limits`.
    pub(crate) fn unwrap(crypt: &Crypt, uuid: &[u8; 16], passphrase: &[u8], limits: scrypt::Limits) -> Result<MasterKey> {
        let mut key = passphrase_key(passphrase, crypt.kdf, limits)?;
        let mut bytes = crypt.wrapped;
        let unwrapped = wrap(&key, uuid, &mut bytes);
        erase(&mut key);
        unwrapped?;
        let master = (bytes[..8] == KEY_MAGIC).then(|| MasterKey(bytes[8..].try_into().unwrap()));
        erase(&mut bytes);
        master.ok_or(Error::Passphrase)
    }

    /// `crypt`'s 40 wrapped bytes: the magic and this key under
    /// `passphrase`'s key.
    pub(crate) fn wrapped(&self, uuid: &[u8; 16], passphrase: &[u8], kdf: Kdf) -> Result<[u8; 40]> {
        let mut key = passphrase_key(passphrase, kdf, limits(kdf)?)?;
        let mut out = [0u8; 40];
        out[..8].copy_from_slice(&KEY_MAGIC);
        out[8..].copy_from_slice(&self.0);
        let wrapped = wrap(&key, uuid, &mut out);
        erase(&mut key);
        wrapped.map(|()| out)
    }
}

/// The limits that admit exactly `kdf`: a caller formatting a volume chose
/// its cost. A cost past `usize` saturates, and scrypt refuses it.
fn limits(kdf: Kdf) -> Result<scrypt::Limits> {
    let (n, r, p) = kdf.values()?;
    let memory = 128 * u128::from(r) * (u128::from(n) + 2);
    let work = u128::from(n) * u128::from(r) * u128::from(p);
    Ok(scrypt::Limits { memory: memory.min(usize::MAX as u128) as usize, work: work.min(u128::from(u64::MAX)) as u64 })
}

/// scrypt of the passphrase with the format's salt: the key that wraps
/// the master key.
fn passphrase_key(passphrase: &[u8], kdf: Kdf, limits: scrypt::Limits) -> Result<[u8; 32]> {
    let (_, r, p) = kdf.values()?;
    let log_n = u8::try_from(kdf.log_n).ok().filter(|&n| n < 32).ok_or(Error::Unsupported("scrypt N past 2^31"))?;
    let params = scrypt::Params::new(log_n, r, p, limits).map_err(|_| Error::Unsupported("scrypt parameters past RFC 7914's or the caller's limits"))?;
    let mut workspace = vec![0u32; params.workspace_len()];
    let mut key = [0u8; 32];
    scrypt::derive(passphrase, SALT, params, &mut workspace, &mut key).map_err(|_| Error::Unsupported("scrypt parameters"))?;
    Ok(key)
}

/// The XOR of the 40 key bytes with `key`'s keystream at the fixed
/// nonce the internal UUID gives.
fn wrap(key: &[u8; 32], uuid: &[u8; 16], bytes: &mut [u8; 40]) -> Result<()> {
    let mut nonce = [0u8; 12];
    nonce[4..].copy_from_slice(&uuid[..8]);
    chacha::xor(key, &nonce, 0, bytes).map_err(|_| Error::Corrupt("keystream past its counter"))
}

/// Every nonce the engine encrypts under, with what it encrypted: a nonce
/// used twice under one key over two plaintexts leaks both (§3.5). Tests
/// read it; the engine only records.
#[cfg(test)]
pub(crate) mod oracle {
    use alloc::collections::BTreeMap;
    use alloc::vec::Vec;
    use core::cell::RefCell;

    use toyos_crypto::hash::Sha256;

    use super::{MasterKey, Nonce};

    /// (key digest, nonce) to the digest of each plaintext seen under it.
    type Seen = BTreeMap<([u8; 32], [u32; 4]), Vec<[u8; 32]>>;

    std::thread_local! {
        static SEEN: RefCell<Seen> = RefCell::default();
    }

    pub(crate) fn record(key: &MasterKey, nonce: Nonce, plaintext: &[u8]) {
        let entry = (Sha256::digest(&key.0).unwrap(), nonce.0);
        let digest = Sha256::digest(plaintext).unwrap();
        SEEN.with(|s| {
            let mut s = s.borrow_mut();
            let seen = s.entry(entry).or_default();
            if !seen.contains(&digest) {
                seen.push(digest);
            }
        });
    }

    /// The nonces used over more than one plaintext.
    pub(crate) fn reused() -> Vec<[u32; 4]> {
        SEEN.with(|s| s.borrow().iter().filter(|(_, d)| d.len() > 1).map(|((_, n), _)| *n).collect())
    }

    pub(crate) fn reset() {
        SEEN.with(|s| s.borrow_mut().clear());
    }

    /// How many nonces have been used.
    pub(crate) fn nonces() -> usize {
        SEEN.with(|s| s.borrow().len())
    }
}
