//! Algorithm negotiation (RFC 4253 §7.1), the curve25519-sha256 exchange hash
//! (RFC 8731 §3) and key derivation (RFC 4253 §7.2).
//!
//! **One algorithm per slot, and the key exchange a list.** The server offers
//! `ssh-ed25519`, `chacha20-poly1305@openssh.com` and no compression; the
//! cipher is an AEAD, so the MAC lists are empty and never negotiated. The key
//! exchange methods are [`METHODS`], chosen in the client's order, so a second
//! method is one more entry and one more arm of [`Method`].

use ring::digest;

use crate::hostkey;
use crate::wire::{put_bool, put_string, put_u32, NameList, Reader, Refusal};

/// A key exchange method this server runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Method {
    Curve25519Sha256,
}

impl Method {
    fn name(self) -> &'static str {
        match self {
            Self::Curve25519Sha256 => "curve25519-sha256",
        }
    }
}

/// The methods offered, most preferred first.
pub(crate) const METHODS: &[Method] = &[Method::Curve25519Sha256];

/// Strict key exchange (OpenSSH `PROTOCOL` §1.10): the server's marker, and the
/// client's, without which no session runs.
const STRICT_SERVER: &str = "kex-strict-s-v00@openssh.com";
const STRICT_CLIENT: &str = "kex-strict-c-v00@openssh.com";

/// The client takes `EXT_INFO` (RFC 8308 §2.1).
const EXT_INFO_CLIENT: &str = "ext-info-c";

pub(crate) const CIPHER: &str = "chacha20-poly1305@openssh.com";
const COMPRESSION: &str = "none";

/// What a KEXINIT pair settled.
pub(crate) struct Negotiated {
    pub(crate) method: Method,
    /// The client takes `EXT_INFO`; read from the first KEXINIT only.
    pub(crate) ext_info: bool,
}

/// The server's KEXINIT payload.
pub(crate) fn kexinit(cookie: [u8; 16]) -> Vec<u8> {
    let mut out = vec![crate::msg::KEXINIT];
    out.extend_from_slice(&cookie);
    let methods: Vec<&str> = METHODS.iter().map(|m| m.name()).chain([STRICT_SERVER]).collect();
    put_string(&mut out, methods.join(",").as_bytes());
    put_string(&mut out, hostkey::ALGORITHM.as_bytes());
    put_string(&mut out, CIPHER.as_bytes());
    put_string(&mut out, CIPHER.as_bytes());
    put_string(&mut out, b"");
    put_string(&mut out, b"");
    put_string(&mut out, COMPRESSION.as_bytes());
    put_string(&mut out, COMPRESSION.as_bytes());
    put_string(&mut out, b"");
    put_string(&mut out, b"");
    put_bool(&mut out, false);
    put_u32(&mut out, 0);
    out
}

/// The client's KEXINIT `payload` against the server's offer. `first` is the
/// session's first exchange, the only one whose markers count.
pub(crate) fn negotiate(payload: &[u8], first: bool) -> Result<Negotiated, Refusal> {
    let mut r = Reader::new(payload);
    r.byte("KEXINIT")?;
    r.array::<16>("cookie")?;
    let kex = r.name_list("kex_algorithms")?;
    let host_key = r.name_list("server_host_key_algorithms")?;
    let cipher_in = r.name_list("encryption_algorithms_client_to_server")?;
    let cipher_out = r.name_list("encryption_algorithms_server_to_client")?;
    r.name_list("mac_algorithms_client_to_server")?;
    r.name_list("mac_algorithms_server_to_client")?;
    let compression_in = r.name_list("compression_algorithms_client_to_server")?;
    let compression_out = r.name_list("compression_algorithms_server_to_client")?;
    r.name_list("languages_client_to_server")?;
    r.name_list("languages_server_to_client")?;
    let guess_follows = r.boolean("first_kex_packet_follows")?;
    r.u32("reserved")?;
    r.end("KEXINIT")?;
    if guess_follows {
        return Err(Refusal::Malformed("a guessed key exchange packet follows the KEXINIT"));
    }
    let method = kex
        .names()
        .find_map(|name| METHODS.iter().copied().find(|m| m.name() == name))
        .ok_or(Refusal::NoCommonAlgorithm("kex_algorithms"))?;
    let need = |list: NameList<'_>, name: &str, field: &'static str| {
        if list.contains(name) {
            Ok(())
        } else {
            Err(Refusal::NoCommonAlgorithm(field))
        }
    };
    need(host_key, hostkey::ALGORITHM, "server_host_key_algorithms")?;
    need(cipher_in, CIPHER, "encryption_algorithms_client_to_server")?;
    need(cipher_out, CIPHER, "encryption_algorithms_server_to_client")?;
    need(compression_in, COMPRESSION, "compression_algorithms_client_to_server")?;
    need(compression_out, COMPRESSION, "compression_algorithms_server_to_client")?;
    if first && !kex.contains(STRICT_CLIENT) {
        return Err(Refusal::NotStrict);
    }
    Ok(Negotiated { method, ext_info: first && kex.contains(EXT_INFO_CLIENT) })
}

/// The inputs of the exchange hash, in RFC 8731 §3's order.
pub(crate) struct Exchange<'a> {
    pub(crate) client_version: &'a [u8],
    pub(crate) server_version: &'a [u8],
    pub(crate) client_kexinit: &'a [u8],
    pub(crate) server_kexinit: &'a [u8],
    pub(crate) host_key: &'a [u8],
    pub(crate) client_public: &'a [u8],
    pub(crate) server_public: &'a [u8],
    /// `K` as an `mpint`, length included.
    pub(crate) secret: &'a [u8],
}

impl Exchange<'_> {
    /// `H`.
    pub(crate) fn hash(&self) -> [u8; 32] {
        let mut data = Vec::new();
        for field in [
            self.client_version,
            self.server_version,
            self.client_kexinit,
            self.server_kexinit,
            self.host_key,
            self.client_public,
            self.server_public,
        ] {
            put_string(&mut data, field);
        }
        data.extend_from_slice(self.secret);
        sha256(&[&data])
    }
}

/// The 64 bytes of key `letter` (RFC 4253 §7.2): `HASH(K || H || letter ||
/// session_id)`, extended by `HASH(K || H || K1)`. `secret` is `K` as an `mpint`.
pub(crate) fn derive(secret: &[u8], hash: &[u8; 32], letter: u8, session_id: &[u8; 32]) -> [u8; 64] {
    let first = sha256(&[secret, hash, &[letter], session_id]);
    let second = sha256(&[secret, hash, &first]);
    let mut key = [0; 64];
    key[..32].copy_from_slice(&first);
    key[32..].copy_from_slice(&second);
    key
}

fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    let mut context = digest::Context::new(&digest::SHA256);
    for part in parts {
        context.update(part);
    }
    context.finish().as_ref().try_into().expect("SHA-256 is 32 bytes")
}
