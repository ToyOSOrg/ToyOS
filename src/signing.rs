//! The key an image is signed with, and the one place a private key is held.
//!
//! **Two keys, chosen by what the image is for.** An image that stays on this
//! Mac — a QEMU guest of `cargo run` or `cargo test`, or a CI run — is signed
//! with a throwaway key minted in this process and never written anywhere:
//! its loader embeds the throwaway's public half, so only this process can
//! make an update it accepts, and the next run mints another. An image that
//! leaves the Mac to be installed on a machine (`--owner-key`,
//! `--update-image`) is signed with the owner's key, read from
//! [`owner_key_path`], and a build that asks for it where there is none is
//! refused by name before anything is built.
//!
//! **The owner's key never reaches the machine and is never printed**: the
//! loader and `/system/bin/update` carry the public half ([`KEY_ENV`] at their
//! compile), the private half is read here and handed to nothing but the
//! signer, and only the public key's fingerprint is ever said. The file is an
//! OpenSSH Ed25519 private key without a passphrase, so `ssh-keygen -Y sign -n
//! toyos-image` over an image's header makes the same signature this does.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use toyos_update::image::{Header, HEADER_BYTES, SIGNATURE_BYTES, SIGNED_BYTES};

/// The variable the loader and `/system/bin/update` take the public key from
/// at compile time: 64 lowercase hex digits.
pub const KEY_ENV: &str = "TOYOS_IMAGE_KEY";

/// The variable naming the owner's private key, where it is not at
/// [`DEFAULT_OWNER_KEY`] under `$HOME`.
pub const OWNER_KEY_ENV: &str = "TOYOS_SIGNING_KEY";

/// Where the owner's key is by default: outside every checkout.
pub const DEFAULT_OWNER_KEY: &str = ".config/toyos/image-signing-key";

/// A key this process signs with.
pub struct Key {
    seed: [u8; 32],
    public: [u8; 32],
    whose: Whose,
}

/// Whose key it is, which is what every line about it says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Whose {
    /// Minted in this process, and gone with it.
    Throwaway,
    /// The owner's, read from this file.
    Owner(PathBuf),
}

impl Key {
    fn from_seed(seed: [u8; 32], whose: Whose) -> Self {
        Self { public: toyos_update::sig::public_of(&seed), seed, whose }
    }

    /// The public key as [`KEY_ENV`] carries it.
    pub fn public_hex(&self) -> String {
        self.public.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn public(&self) -> [u8; 32] {
        self.public
    }

    pub fn whose(&self) -> &Whose {
        &self.whose
    }

    /// `SHA256:<base64>` of the OpenSSH public key blob, as `ssh-keygen -l`
    /// prints it: the one name for this key a line may carry.
    pub fn fingerprint(&self) -> String {
        let digest = toyos_update::sha256(&public_blob(&self.public));
        format!("SHA256:{}", base64_encode(&digest).trim_end_matches('='))
    }

    /// The header naming these sections, and its signature: a slot's signed
    /// header.
    pub fn sign(&self, header: &Header) -> [u8; SIGNED_BYTES] {
        let bytes = header.encode();
        let signature: [u8; SIGNATURE_BYTES] = toyos_update::sig::sign(&self.seed, &bytes);
        let mut out = [0u8; SIGNED_BYTES];
        out[..HEADER_BYTES].copy_from_slice(&bytes);
        out[HEADER_BYTES..].copy_from_slice(&signature);
        out
    }

    /// A key from a seed the caller chose, for a test that needs a second,
    /// wrong key or a fixed one.
    pub fn throwaway_from(seed: [u8; 32]) -> Self {
        Self::from_seed(seed, Whose::Throwaway)
    }

    /// A fresh throwaway key.
    pub fn mint() -> Self {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).expect("the operating system's randomness");
        Self::throwaway_from(seed)
    }
}

static KEY: OnceLock<Key> = OnceLock::new();

/// This process's key: the owner's where [`use_owner`] was asked first, a
/// throwaway otherwise.
pub fn key() -> &'static Key {
    KEY.get_or_init(Key::mint)
}

/// Sign everything this process builds with the owner's key, or say why not.
/// Asked before anything is built, so no image is signed by two keys.
pub fn use_owner() -> Result<&'static Key, String> {
    let path = owner_key_path()?;
    let owner = read_owner_key(&path)?;
    if KEY.set(owner).is_err() {
        return Err("this process already signs with another key".into());
    }
    Ok(key())
}

/// Where the owner's key is: [`OWNER_KEY_ENV`], else [`DEFAULT_OWNER_KEY`]
/// under `$HOME`.
pub fn owner_key_path() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os(OWNER_KEY_ENV) {
        return Ok(PathBuf::from(path));
    }
    let home = std::env::var_os("HOME").ok_or("no $HOME, and no TOYOS_SIGNING_KEY names the owner's key")?;
    Ok(PathBuf::from(home).join(DEFAULT_OWNER_KEY))
}

/// The owner's key at `path`, refused by name where it is not one.
fn read_owner_key(path: &Path) -> Result<Key, String> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        format!(
            "the owner's image-signing key is not at {} ({e}). Mint one with \
             `cargo run -- --signing-key-new` (it is written there and never into a checkout), \
             or point {OWNER_KEY_ENV} at an OpenSSH Ed25519 key without a passphrase",
            path.display()
        )
    })?;
    let seed = openssh_seed(&text).map_err(|why| format!("{}: {why}", path.display()))?;
    Ok(Key::from_seed(seed, Whose::Owner(path.to_path_buf())))
}

/// Mint the owner's key at [`owner_key_path`], refusing to replace one: a key
/// replaced is every installed machine refusing the next update.
pub fn mint_owner_key() -> Result<(PathBuf, String), String> {
    let path = owner_key_path()?;
    if path.exists() {
        return Err(format!("{} already holds a key, and it is not replaced", path.display()));
    }
    let key = Key::mint();
    let dir = path.parent().ok_or_else(|| format!("{} has no directory", path.display()))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let text = openssh_private(&key.seed, &key.public);
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    file.write_all(text.as_bytes()).map_err(|e| format!("{}: {e}", path.display()))?;
    file.sync_all().map_err(|e| format!("{}: {e}", path.display()))?;
    Ok((path, key.fingerprint()))
}

/// `string ssh-ed25519 | string key`, the OpenSSH public key blob.
fn public_blob(public: &[u8; 32]) -> Vec<u8> {
    let mut out = Vec::new();
    put_string(&mut out, b"ssh-ed25519");
    put_string(&mut out, public);
    out
}

fn put_string(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
}

fn take_string<'a>(bytes: &mut &'a [u8]) -> Result<&'a [u8], String> {
    let len = bytes.get(..4).ok_or("the key ends inside a length")?;
    let len = u32::from_be_bytes(len.try_into().expect("four bytes")) as usize;
    let s = bytes.get(4..4 + len).ok_or("the key ends inside a field")?;
    *bytes = &bytes[4 + len..];
    Ok(s)
}

const BEGIN: &str = "-----BEGIN OPENSSH PRIVATE KEY-----";
const END: &str = "-----END OPENSSH PRIVATE KEY-----";
const MAGIC: &[u8] = b"openssh-key-v1\0";

/// The Ed25519 seed an unencrypted `openssh-key-v1` file holds
/// (OpenSSH's `PROTOCOL.key`).
fn openssh_seed(text: &str) -> Result<[u8; 32], String> {
    let body = text
        .trim()
        .strip_prefix(BEGIN)
        .and_then(|t| t.strip_suffix(END))
        .ok_or("not an OpenSSH private key")?;
    let blob = base64_decode(body)?;
    let mut rest = blob.strip_prefix(MAGIC).ok_or("not an openssh-key-v1 key")?;
    if take_string(&mut rest)? != b"none" || take_string(&mut rest)? != b"none" {
        return Err("the key is encrypted, and a build cannot ask for its passphrase".into());
    }
    take_string(&mut rest)?;
    let count = rest.get(..4).ok_or("no key count")?;
    if u32::from_be_bytes(count.try_into().expect("four bytes")) != 1 {
        return Err("the file holds more than one key".into());
    }
    rest = &rest[4..];
    let mut public = take_string(&mut rest)?;
    let mut private = take_string(&mut rest)?;
    if take_string(&mut public)? != b"ssh-ed25519" {
        return Err("the key is not Ed25519".into());
    }
    let public: [u8; 32] = take_string(&mut public)?.try_into().map_err(|_| "a public key that is not 32 bytes")?;
    let checks = private.get(..8).ok_or("no check words")?;
    if checks[..4] != checks[4..] {
        return Err("the check words disagree, which is a key that was not decrypted".into());
    }
    private = &private[8..];
    if take_string(&mut private)? != b"ssh-ed25519" {
        return Err("the private half is not Ed25519".into());
    }
    if take_string(&mut private)? != public {
        return Err("the private half names another public key".into());
    }
    let pair = take_string(&mut private)?;
    let seed: [u8; 32] = pair.get(..32).and_then(|s| s.try_into().ok()).ok_or("a private key that is not 64 bytes")?;
    if toyos_update::sig::public_of(&seed) != public {
        return Err("the seed does not make the public key the file names".into());
    }
    Ok(seed)
}

/// An unencrypted `openssh-key-v1` file for `seed`, which OpenSSH reads.
fn openssh_private(seed: &[u8; 32], public: &[u8; 32]) -> String {
    let mut blob = MAGIC.to_vec();
    put_string(&mut blob, b"none");
    put_string(&mut blob, b"none");
    put_string(&mut blob, b"");
    blob.extend_from_slice(&1u32.to_be_bytes());
    put_string(&mut blob, &public_blob(public));
    let mut private = Vec::new();
    let mut check = [0u8; 4];
    getrandom::fill(&mut check).expect("the operating system's randomness");
    private.extend_from_slice(&check);
    private.extend_from_slice(&check);
    put_string(&mut private, b"ssh-ed25519");
    put_string(&mut private, public);
    let mut pair = seed.to_vec();
    pair.extend_from_slice(public);
    put_string(&mut private, &pair);
    put_string(&mut private, b"toyos-image");
    let mut pad = 1u8;
    while !private.len().is_multiple_of(8) {
        private.push(pad);
        pad += 1;
    }
    put_string(&mut blob, &private);
    let encoded = base64_encode(&blob);
    let mut text = format!("{BEGIN}\n");
    for line in encoded.as_bytes().chunks(70) {
        text.push_str(std::str::from_utf8(line).expect("base64 is ASCII"));
        text.push('\n');
    }
    text.push_str(END);
    text.push('\n');
    text
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |acc, (i, &b)| acc | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn base64_decode(text: &str) -> Result<Vec<u8>, String> {
    let digits: Vec<u8> = text.bytes().filter(|c| !c.is_ascii_whitespace() && *c != b'=').collect();
    let mut out = Vec::new();
    for chunk in digits.chunks(4) {
        if chunk.len() == 1 {
            return Err("base64 that ends one digit into a group".into());
        }
        let mut n = 0u32;
        for (i, c) in chunk.iter().enumerate() {
            let v = ALPHABET.iter().position(|a| a == c).ok_or_else(|| format!("{c:#x} is not base64"))?;
            n |= (v as u32) << (18 - 6 * i);
        }
        out.extend_from_slice(&n.to_be_bytes()[1..chunk.len()]);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A key this module writes is one it reads back, and one it signs with
    /// verifies under the verifier the loader calls.
    #[test]
    fn an_owner_key_reads_back_and_signs_what_the_loader_verifies() {
        let key = Key::mint();
        let text = openssh_private(&key.seed, &key.public);
        assert_eq!(openssh_seed(&text), Ok(key.seed));
        let header = Header::of(3, b"k", b"c", &[0; 4096]);
        let signed = key.sign(&header);
        let sig: [u8; 64] = signed[HEADER_BYTES..].try_into().unwrap();
        let bytes: [u8; HEADER_BYTES] = signed[..HEADER_BYTES].try_into().unwrap();
        assert_eq!(toyos_update::sig::verify(&key.public, &bytes, &sig), Ok(()));
        assert!(Key::mint().public != key.public, "two mints made one key");
    }

    /// Everything that is not an unencrypted Ed25519 `openssh-key-v1` key is
    /// refused, each by the word for it; the fingerprint is the form
    /// `ssh-keygen -l` prints.
    #[test]
    fn a_key_that_is_not_one_is_refused_by_name() {
        let key = Key::throwaway_from([7; 32]);
        assert!(openssh_seed("not a key").unwrap_err().contains("not an OpenSSH private key"));
        let armour = |blob: &[u8]| format!("{BEGIN}\n{}\n{END}\n", base64_encode(blob));
        let mut encrypted = MAGIC.to_vec();
        put_string(&mut encrypted, b"aes256-ctr");
        put_string(&mut encrypted, b"bcrypt");
        assert!(openssh_seed(&armour(&encrypted)).unwrap_err().contains("encrypted"));
        let text = openssh_private(&key.seed, &key.public);
        let body: String = text.lines().filter(|l| !l.starts_with("-----")).collect();
        let mut blob = base64_decode(&body).unwrap();
        // The seed's last byte, inside the private half: a seed that does not
        // make the public key the file names.
        let at = blob.windows(32).position(|w| w == key.seed).expect("the seed is in the file") + 31;
        blob[at] ^= 1;
        assert!(openssh_seed(&armour(&blob)).unwrap_err().contains("does not make the public key"));
        assert!(key.fingerprint().starts_with("SHA256:"));
        assert_eq!(base64_decode(&base64_encode(b"any carnal pleas")).unwrap(), b"any carnal pleas");
        assert_eq!(base64_encode(b"Man"), "TWFu");
        assert_eq!(base64_encode(b"Ma"), "TWE=");
    }
}
