//! User authentication before a session exists (RFC 4252): the
//! `ssh-userauth` service and the `publickey` method with `ssh-ed25519` keys
//! (RFC 8709), nothing else.
//!
//! **The only way out of here is a verified signature.** [`Verified`] is what
//! the connection layer is built from, and its one constructor is the arm below
//! that has just had `ring` verify the client's signature over this session's
//! identifier with a key the authorizer named. [`PreAuth`] holds no channel
//! table and cannot name a channel.
//!
//! **An offered key the authorizer does not name is refused at the offer**, so
//! the client is never asked to sign with it; a signed request asks the
//! authorizer again. Every other method and every other key algorithm — RSA
//! among them — is refused by name. [`ATTEMPTS`] failures end the session.

use ring::signature::{UnparsedPublicKey, ED25519};

use crate::hostkey;
use crate::wire::{put_bool, put_string, Reader, Refusal};
use crate::{msg, Authorizer, Declined, Event};

/// Failed attempts before the session ends: OpenSSH's `MaxAuthTries`.
pub(crate) const ATTEMPTS: u32 = 6;

const SERVICE: &str = "ssh-userauth";
const CONNECTION: &str = "ssh-connection";
const PUBLICKEY: &str = "publickey";
const USER_CAP: usize = 256;
const BLOB_CAP: usize = 16 * 1024;

/// Authentication not yet done.
pub(crate) struct PreAuth {
    service: bool,
    failures: u32,
}

/// A client whose signature verified, and the user it logged in as.
pub(crate) struct Verified {
    user: String,
}

impl Verified {
    pub(crate) fn user(&self) -> &str {
        &self.user
    }
}

impl PreAuth {
    pub(crate) fn new() -> Self {
        Self { service: false, failures: 0 }
    }

    /// One message of the authentication layer.
    pub(crate) fn handle(
        &mut self,
        payload: &[u8],
        session_id: &[u8; 32],
        authorizer: &mut impl Authorizer,
        out: &mut Vec<Vec<u8>>,
        events: &mut Vec<Event>,
    ) -> Result<Option<Verified>, Refusal> {
        let mut r = Reader::new(payload);
        match (r.byte("message")?, self.service) {
            (msg::SERVICE_REQUEST, false) => {
                if r.text("service name", 64)? != SERVICE {
                    return Err(Refusal::Malformed("a service other than ssh-userauth before authentication"));
                }
                r.end("SERVICE_REQUEST")?;
                self.service = true;
                let mut accept = vec![msg::SERVICE_ACCEPT];
                put_string(&mut accept, SERVICE.as_bytes());
                out.push(accept);
                Ok(None)
            }
            (msg::USERAUTH_REQUEST, true) => self.request(r, payload, session_id, authorizer, out, events),
            (message, _) => Err(Refusal::Unexpected { phase: "authentication", message }),
        }
    }

    fn request(
        &mut self,
        mut r: Reader<'_>,
        payload: &[u8],
        session_id: &[u8; 32],
        authorizer: &mut impl Authorizer,
        out: &mut Vec<Vec<u8>>,
        events: &mut Vec<Event>,
    ) -> Result<Option<Verified>, Refusal> {
        let user = r.text("user name", USER_CAP)?;
        if r.text("service name", 64)? != CONNECTION {
            return Err(Refusal::Malformed("authentication for a service other than ssh-connection"));
        }
        let declined = match r.name("method name")? {
            "none" => {
                r.end("USERAUTH_REQUEST none")?;
                out.push(failure());
                return Ok(None);
            }
            PUBLICKEY => {
                let signed = r.boolean("has signature")?;
                let algorithm = r.name("public key algorithm name")?;
                let blob = r.string("public key blob", BLOB_CAP)?;
                // Everything up to the signature is what the client signed.
                let unsigned_len = payload.len() - r.rest_len();
                let signature = if signed { Some(r.string("signature", BLOB_CAP)?) } else { None };
                r.end("USERAUTH_REQUEST publickey")?;
                if algorithm != hostkey::ALGORITHM {
                    Declined::KeyAlgorithm(algorithm.to_string())
                } else {
                    let key = ed25519_key(blob)?;
                    if !authorizer.authorizes(user, &key) {
                        Declined::Key { user: user.to_string(), fingerprint: hostkey::fingerprint(&key) }
                    } else if let Some(signature) = signature {
                        let signature = ed25519_signature(signature)?;
                        let mut data = Vec::new();
                        put_string(&mut data, session_id);
                        data.extend_from_slice(payload.get(..unsigned_len).unwrap_or_default());
                        if UnparsedPublicKey::new(&ED25519, key).verify(&data, &signature).is_ok() {
                            out.push(vec![msg::USERAUTH_SUCCESS]);
                            return Ok(Some(Verified { user: user.to_string() }));
                        }
                        Declined::Signature { user: user.to_string() }
                    } else {
                        let mut ok = vec![msg::USERAUTH_PK_OK];
                        put_string(&mut ok, algorithm.as_bytes());
                        put_string(&mut ok, blob);
                        out.push(ok);
                        return Ok(None);
                    }
                }
            }
            method => Declined::Method(method.to_string()),
        };
        events.push(Event::Declined(declined));
        self.failures += 1;
        if self.failures >= ATTEMPTS {
            return Err(Refusal::TooManyAttempts);
        }
        out.push(failure());
        Ok(None)
    }
}

/// `USERAUTH_FAILURE`: `publickey` can continue, and nothing succeeded in part.
fn failure() -> Vec<u8> {
    let mut out = vec![msg::USERAUTH_FAILURE];
    put_string(&mut out, PUBLICKEY.as_bytes());
    put_bool(&mut out, false);
    out
}

/// The key of an `ssh-ed25519` public key blob (RFC 8709 §4).
fn ed25519_key(blob: &[u8]) -> Result<[u8; 32], Refusal> {
    let mut r = Reader::new(blob);
    if r.name("key blob's algorithm")? != hostkey::ALGORITHM {
        return Err(Refusal::Malformed("a key blob of another algorithm than its request names"));
    }
    let key = r.fixed("Ed25519 public key")?;
    r.end("public key blob")?;
    Ok(key)
}

/// The signature of an `ssh-ed25519` signature blob (RFC 8709 §6).
fn ed25519_signature(blob: &[u8]) -> Result<[u8; 64], Refusal> {
    let mut r = Reader::new(blob);
    if r.name("signature's algorithm")? != hostkey::ALGORITHM {
        return Err(Refusal::Malformed("a signature of another algorithm than its key"));
    }
    let signature = r.fixed("Ed25519 signature")?;
    r.end("signature blob")?;
    Ok(signature)
}
