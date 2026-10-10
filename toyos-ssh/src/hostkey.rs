//! An Ed25519 key in OpenSSH's unencrypted `openssh-key-v1` file
//! (`PROTOCOL.key`), read and written, and its `SHA256:` fingerprint: the
//! server's host key, and the one parser the build's image-signing key is read
//! with too.

use ring::signature::{Ed25519KeyPair, KeyPair};

use crate::base64;
use crate::wire::put_string;

const BEGIN: &str = "-----BEGIN OPENSSH PRIVATE KEY-----";
const END: &str = "-----END OPENSSH PRIVATE KEY-----";
const MAGIC: &[u8] = b"openssh-key-v1\0";

/// The one host key algorithm.
pub(crate) const ALGORITHM: &str = "ssh-ed25519";

/// The key the server proves itself with.
pub struct HostKey {
    pair: Ed25519KeyPair,
}

impl HostKey {
    pub fn from_seed(seed: &[u8; 32]) -> Self {
        Self { pair: Ed25519KeyPair::from_seed_unchecked(seed).expect("any 32 bytes are an Ed25519 seed") }
    }

    /// The key an `openssh-key-v1` file holds, refused by name where it is
    /// not one unencrypted Ed25519 key.
    pub fn from_openssh(text: &str) -> Result<Self, &'static str> {
        openssh_seed(text).map(|seed| Self::from_seed(&seed))
    }

    pub fn public(&self) -> [u8; 32] {
        self.pair.public_key().as_ref().try_into().expect("an Ed25519 public key is 32 bytes")
    }

    /// `K_S`, the public key blob the exchange hash covers.
    pub(crate) fn blob(&self) -> Vec<u8> {
        public_blob(&self.public())
    }

    /// `string ssh-ed25519 | string signature` over `data` (RFC 8709 §6).
    pub(crate) fn sign(&self, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        put_string(&mut out, ALGORITHM.as_bytes());
        put_string(&mut out, self.pair.sign(data).as_ref());
        out
    }
}

/// `SHA256:<base64>` of the public key blob, as `ssh-keygen -l` prints it.
pub fn fingerprint(public: &[u8; 32]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, &public_blob(public));
    format!("SHA256:{}", base64::encode(digest.as_ref()).trim_end_matches('='))
}

/// `string ssh-ed25519 | string key`, the OpenSSH public key blob (RFC 8709 §4).
pub(crate) fn public_blob(public: &[u8; 32]) -> Vec<u8> {
    let mut out = Vec::new();
    put_string(&mut out, ALGORITHM.as_bytes());
    put_string(&mut out, public);
    out
}

fn take_string<'a>(bytes: &mut &'a [u8]) -> Result<&'a [u8], &'static str> {
    let (len, rest) = bytes.split_first_chunk::<4>().ok_or("the key ends inside a length")?;
    let len = u32::from_be_bytes(*len) as usize;
    if len > rest.len() {
        return Err("the key ends inside a field");
    }
    let (s, rest) = rest.split_at(len);
    *bytes = rest;
    Ok(s)
}

/// The Ed25519 seed an unencrypted `openssh-key-v1` file holds, checked to
/// make the public key the file names.
pub fn openssh_seed(text: &str) -> Result<[u8; 32], &'static str> {
    let body = text.trim().strip_prefix(BEGIN).and_then(|t| t.strip_suffix(END)).ok_or("not an OpenSSH private key")?;
    let blob = base64::decode(body)?;
    let mut rest = blob.strip_prefix(MAGIC).ok_or("not an openssh-key-v1 key")?;
    if take_string(&mut rest)? != b"none" || take_string(&mut rest)? != b"none" {
        return Err("the key is encrypted, and nothing here can ask for its passphrase");
    }
    take_string(&mut rest)?;
    let (count, after) = rest.split_first_chunk::<4>().ok_or("no key count")?;
    if u32::from_be_bytes(*count) != 1 {
        return Err("the file holds more than one key");
    }
    rest = after;
    let mut public = take_string(&mut rest)?;
    let mut private = take_string(&mut rest)?;
    if take_string(&mut public)? != ALGORITHM.as_bytes() {
        return Err("the key is not Ed25519");
    }
    let public: [u8; 32] = take_string(&mut public)?.try_into().map_err(|_| "a public key that is not 32 bytes")?;
    let (checks, after) = private.split_first_chunk::<8>().ok_or("no check words")?;
    if checks[..4] != checks[4..] {
        return Err("the check words disagree, which is a key that was not decrypted");
    }
    private = after;
    if take_string(&mut private)? != ALGORITHM.as_bytes() {
        return Err("the private half is not Ed25519");
    }
    if take_string(&mut private)? != public {
        return Err("the private half names another public key");
    }
    let pair = take_string(&mut private)?;
    let seed: [u8; 32] = pair.get(..32).and_then(|s| s.try_into().ok()).ok_or("a private key that is not 64 bytes")?;
    if pair.len() != 64 {
        return Err("a private key that is not 64 bytes");
    }
    Ed25519KeyPair::from_seed_and_public_key(&seed, &public)
        .map_err(|_| "the seed does not make the public key the file names")?;
    Ok(seed)
}

/// An unencrypted `openssh-key-v1` file for `seed`, which OpenSSH reads.
/// `check` is the file's random check word.
pub fn openssh_private(seed: &[u8; 32], check: [u8; 4], comment: &str) -> String {
    let public = HostKey::from_seed(seed).public();
    let mut blob = MAGIC.to_vec();
    put_string(&mut blob, b"none");
    put_string(&mut blob, b"none");
    put_string(&mut blob, b"");
    blob.extend_from_slice(&1u32.to_be_bytes());
    put_string(&mut blob, &public_blob(&public));
    let mut private = Vec::new();
    private.extend_from_slice(&check);
    private.extend_from_slice(&check);
    put_string(&mut private, ALGORITHM.as_bytes());
    put_string(&mut private, &public);
    let mut pair = seed.to_vec();
    pair.extend_from_slice(&public);
    put_string(&mut private, &pair);
    put_string(&mut private, comment.as_bytes());
    let mut pad = 1u8;
    while !private.len().is_multiple_of(8) {
        private.push(pad);
        pad += 1;
    }
    put_string(&mut blob, &private);
    let encoded = base64::encode(&blob);
    let mut text = format!("{BEGIN}\n");
    for line in encoded.as_bytes().chunks(70) {
        text.push_str(std::str::from_utf8(line).expect("base64 is ASCII"));
        text.push('\n');
    }
    text.push_str(END);
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Everything that is not an unencrypted Ed25519 `openssh-key-v1` key is
    /// refused, each by the word for it.
    #[test]
    fn a_key_that_is_not_one_is_refused_by_name() {
        let seed = [7; 32];
        assert_eq!(openssh_seed("not a key"), Err("not an OpenSSH private key"));
        let armour = |blob: &[u8]| format!("{BEGIN}\n{}\n{END}\n", base64::encode(blob));
        let mut encrypted = MAGIC.to_vec();
        put_string(&mut encrypted, b"aes256-ctr");
        put_string(&mut encrypted, b"bcrypt");
        assert!(openssh_seed(&armour(&encrypted)).unwrap_err().contains("encrypted"));
        let text = openssh_private(&seed, [1, 2, 3, 4], "test");
        assert_eq!(openssh_seed(&text), Ok(seed));
        let body: String = text.lines().filter(|l| !l.starts_with("-----")).collect();
        let blob = base64::decode(&body).unwrap();
        // The seed's last byte, inside the private half: a seed that does not
        // make the public key the file names.
        let mut bent = blob.clone();
        let at = bent.windows(32).position(|w| w == seed).expect("the seed is in the file") + 31;
        bent[at] ^= 1;
        assert_eq!(openssh_seed(&armour(&bent)), Err("the seed does not make the public key the file names"));
        // Every cut of the file is refused, never a panic.
        for end in 0..blob.len() {
            assert!(openssh_seed(&armour(&blob[..end])).is_err(), "cut at {end}");
        }
    }

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap()).collect()
    }

    /// RFC 8032 §7.1's first three vectors: the public key a seed makes, and
    /// the signature blob over each message.
    #[test]
    fn rfc_8032_vectors() {
        for (seed, public, message, signature) in [
            (
                "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60",
                "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a",
                "",
                "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b",
            ),
            (
                "4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb",
                "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c",
                "72",
                "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00",
            ),
            (
                "c5aa8df43f9f837bedb7442f31dcb7b166d38535076f094b85ce3a2e0b4458f7",
                "fc51cd8e6218a1a38da47ed00230f0580816ed13ba3303ac5deb911548908025",
                "af82",
                "6291d657deec24024827e69c3abe01a30ce548a284743a445e3680d7db5ac3ac18ff9b538d16f290ae67f760984dc6594a7c15e9716ed28dc027beceea1ec40a",
            ),
        ] {
            let key = HostKey::from_seed(&hex(seed).try_into().unwrap());
            assert_eq!(key.public().to_vec(), hex(public));
            let mut blob = Vec::new();
            put_string(&mut blob, b"ssh-ed25519");
            put_string(&mut blob, &hex(signature));
            assert_eq!(key.sign(&hex(message)), blob);
        }
    }

    /// A file `ssh-keygen -t ed25519` wrote reads, and makes the public key
    /// and fingerprint `ssh-keygen -l` printed for it.
    #[test]
    fn a_key_ssh_keygen_minted_reads_with_its_fingerprint() {
        let text = include_str!("../tests/fixtures/host_ed25519");
        let public = include_str!("../tests/fixtures/host_ed25519.pub");
        let key = HostKey::from_openssh(text).expect("ssh-keygen's key");
        let blob = base64::decode(public.split(' ').nth(1).unwrap()).unwrap();
        assert_eq!(blob, key.blob());
        assert_eq!(fingerprint(&key.public()), include_str!("../tests/fixtures/host_ed25519.fingerprint").trim());
    }
}
