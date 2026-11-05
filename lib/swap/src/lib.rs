//! Replacing a running service's binary: every word `/system/bin/swap` and
//! the supervisor say about it, and every decision the supervisor makes
//! about one. Pure.
//!
//! **A swap is asked by `/system/bin/swap` and done by the supervisor, and nothing else
//! can do either.** `swap` is an ordinary program — `ssh <machine> swap netstack <sha256>
//! <length> < netstack` runs it over a plain `exec` channel, for a login sshserver has already
//! authenticated — and it lends the supervisor the bytes it was given, in a
//! memory region sent with the request over the [`PORT`] the supervisor
//! serves, which the build gate lets no program but [`HOLDER`] receive. The supervisor is the only process holding the system
//! capability, so it is the only one that can stop a service, give its device
//! claims back and start it again holding exactly its manifest row.
//!
//! **The order is fixed and every refusal leaves the old service running.**
//! the supervisor copies the lent bytes into a region of its own, which no
//! other process can write, holds that copy against the requester's
//! [`Digest`] ([`verify`]), answers [`MSG_ACCEPTED`], and waits for the
//! requester to hang up, at most [`HANGUP_MS`]. A swapped service runs from
//! that region, each start of it too: no file is read and no file server
//! is asked. `swap` hangs up once its caller has closed
//! its input, or [`ANSWER_MS`] after its answer — the close is the caller's
//! proof it has the answer: the service being swapped may be
//! the one carrying it, and is not stopped before it has arrived. Then the supervisor
//! stops the old
//! process, starts the new binary, and holds it on probation for
//! [`PROBATION_MS`]. A binary that does not spawn, is refused a device the
//! process it replaces held, or ends inside probation, is [`Word::Failed`], and
//! the supervisor starts the binary it replaced again — owed the same devices.
//!
//! **A swap lasts one boot.** The root volume is the image and is read-only;
//! a replaced binary lives in the supervisor's memory and a reboot runs the
//! image's own. A swap that outlived the boot would be a machine booting something its
//! image does not say.

#![forbid(unsafe_code)]

/// The name the supervisor serves swap requests on — a `supervisor-serve`
/// record — which the manifest names because its holder starts only in a login
/// session.
pub const PORT: &str = toyos_manifest::SWAP_PORT;

/// The endowment label [`PORT`] reaches its holder under: a namespace holding
/// that one name, and never an entry of the holder's `svc` namespace — which
/// std hands, duplicated, to every program the holder spawns directly, so a
/// name in it is a name whatever the holder runs undeclared would hold too.
pub const LABEL: &str = "swap";

/// The one program a build may let receive [`PORT`]: `/system/bin/swap`,
/// which a login runs and nothing starts at boot.
pub const HOLDER: &str = "swap";

/// The largest binary a swap carries. A bound on the memory `swap` and the supervisor
/// each spend on one request; a service binary in this tree is a few megabytes.
pub const MAX_BINARY_BYTES: u64 = 64 * 1024 * 1024;

/// How long a new binary must keep running before the supervisor calls it in service.
///
/// **Policy, and what "fails to start" means here**: a service in this tree has
/// no readiness message, so a binary that ends inside this window is one that
/// did not start, and one that ends after it is a service that died.
pub const PROBATION_MS: u64 = 5_000;

/// How long `swap` waits for its caller to close its input once it has
/// answered — the caller's proof that it has the answer, and its word that the
/// swap may go — before it hangs up on the supervisor anyway.
pub const ANSWER_MS: u64 = 2_000;

/// How long the supervisor waits for the requester to hang up once it has answered
/// [`MSG_ACCEPTED`]. The hang-up is the go; this bounds a requester that never
/// sends one, and is wider than [`ANSWER_MS`] so a requester that waited for
/// its client is never overtaken.
pub const HANGUP_MS: u64 = 2 * ANSWER_MS;

const _: () = assert!(HANGUP_MS > ANSWER_MS);

/// The request, `swap` to the supervisor: [`Request::encode`]'s bytes.
pub const MSG_SWAP: u32 = 1;
/// The supervisor's answer: verified and kept; the payload is the digest, in hex.
pub const MSG_ACCEPTED: u32 = 2;
/// The supervisor's answer: refused, and the old service untouched; the payload is why.
pub const MSG_REFUSED: u32 = 3;

/// A SHA-256 digest.
pub type Digest = [u8; 32];

/// The digest of `bytes`.
pub fn digest(bytes: &[u8]) -> Digest {
    // FIPS 180-4 bounds a message at 2^61 bytes; a service binary is far
    // below it.
    toyos_crypto::hash::Sha256::digest(bytes).expect("a binary below SHA-256's 2^61-byte bound")
}

pub fn hex(digest: &Digest) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Sixty-four lowercase hex digits, or `None`.
pub fn parse_hex(text: &str) -> Option<Digest> {
    if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

/// Whether `name` can be a service's key: the manifest's own program-key bound,
/// and nothing that could make a path component other than itself.
pub fn is_service_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= toyos_manifest::MAX_PROGRAM_NAME
        && name != "."
        && name != ".."
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// Why a swap did not happen. Every one of them leaves the old service running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The request does not decode.
    Malformed(String),
    /// Nothing the supervisor started at boot has this name.
    NotAService(String),
    /// The supervisor started it, and it has ended: its port is closed for good.
    NotRunning(String),
    /// Another swap has not finished.
    Busy(String),
    /// The lent region is not one the request describes.
    Unreadable(String),
    TooLarge(u64),
    /// The bytes are not the ones the requester named.
    Mismatch { got: Digest, want: Digest },
    /// The service already runs this binary.
    AlreadyRuns(Digest),
    /// The supervisor could not make the region a service is started from.
    Install(String),
    /// This process was given no [`PORT`]: the image does not let it swap.
    NoAuthority,
}

impl core::fmt::Display for Refusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Malformed(why) => write!(f, "the request is malformed: {why}"),
            Self::NotAService(name) => write!(f, "{name} is no service the supervisor started at boot"),
            Self::NotRunning(name) => write!(f, "{name} has ended and its port is closed"),
            Self::Busy(name) => write!(f, "a swap of {name} has not finished"),
            Self::Unreadable(why) => write!(f, "the lent binary cannot be read: {why}"),
            Self::TooLarge(len) => {
                write!(f, "{len} bytes is more than the {MAX_BINARY_BYTES} a swap carries")
            }
            Self::Mismatch { got, want } => {
                write!(f, "the binary hashes to {} and the request names {}", hex(got), hex(want))
            }
            Self::AlreadyRuns(digest) => write!(f, "the service already runs sha256 {}", hex(digest)),
            Self::Install(why) => write!(f, "the binary could not be kept: {why}"),
            Self::NoAuthority => write!(f, "this process holds no `{PORT}` connector"),
        }
    }
}

/// The bytes are the ones the requester named, or the refusal saying which
/// they are.
pub fn verify(bytes: &[u8], want: &Digest) -> Result<(), Refusal> {
    let got = digest(bytes);
    if got != *want {
        return Err(Refusal::Mismatch { got, want: *want });
    }
    Ok(())
}

/// The request `swap` sends the supervisor: the service, the digest the client
/// named and the binary's length. The binary is the one region the request's
/// frame carries, at least that long.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub service: String,
    pub digest: Digest,
    pub len: u64,
}

impl Request {
    /// `digest ‖ len (little-endian u64) ‖ service`.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(40 + self.service.len());
        out.extend_from_slice(&self.digest);
        out.extend_from_slice(&self.len.to_le_bytes());
        out.extend_from_slice(self.service.as_bytes());
        out
    }

    /// Everything in it is the requester's claim: a name that is not a
    /// service name and a length past [`MAX_BINARY_BYTES`] are refused here,
    /// before the supervisor looks anything up or reads anything.
    pub fn decode(bytes: &[u8]) -> Result<Self, Refusal> {
        if bytes.len() < 40 {
            return Err(Refusal::Malformed(format!("{} bytes is shorter than a digest and a length", bytes.len())));
        }
        let (digest, rest) = bytes.split_at(32);
        let digest: Digest = digest.try_into().expect("split at 32");
        let (len, service) = rest.split_at(8);
        let len = u64::from_le_bytes(len.try_into().expect("split at 8"));
        let service = core::str::from_utf8(service).map_err(|_| Refusal::Malformed("the name is not UTF-8".into()))?;
        if !is_service_name(service) {
            return Err(Refusal::Malformed(format!("{service:?} is not a service name")));
        }
        if len == 0 {
            return Err(Refusal::Malformed("an empty binary".into()));
        }
        if len > MAX_BINARY_BYTES {
            return Err(Refusal::TooLarge(len));
        }
        Ok(Self { service: service.to_string(), digest, len })
    }
}

/// What the supervisor says about a swap, in the one line form every reader matches:
/// `supervisor: swap <service>: <word>: <detail>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Word {
    Refused,
    /// The bytes matched and are kept; the requester has been answered.
    Accepted,
    /// The old process is being stopped.
    Stopping,
    /// The new binary is running and on probation.
    Started,
    /// Probation passed: the new binary is the service.
    InService,
    /// The new binary did not spawn, was refused a device the process it
    /// replaces held, or ended on probation.
    Failed,
    /// The binary it replaced is running again.
    Restored,
    /// Neither binary runs, and the service's port is closed.
    Gone,
}

const WORDS: &[(Word, &str)] = &[
    (Word::Refused, "refused"),
    (Word::Accepted, "accepted"),
    (Word::Stopping, "stopping"),
    (Word::Started, "started"),
    (Word::InService, "in service"),
    (Word::Failed, "failed"),
    (Word::Restored, "restored"),
    (Word::Gone, "gone"),
];

impl Word {
    pub fn as_str(self) -> &'static str {
        WORDS.iter().find(|(w, _)| *w == self).map(|(_, s)| *s).expect("every word is spelled")
    }
}

pub fn said(service: &str, word: Word, detail: &str) -> String {
    format!("supervisor: swap {service}: {}: {detail}", word.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    /// FIPS 180-2's own example, so the digest here is SHA-256 and not a
    /// digest that agrees with itself.
    #[test]
    fn the_digest_is_sha256() {
        assert_eq!(hex(&digest(b"abc")), ABC);
        assert_eq!(parse_hex(ABC), Some(digest(b"abc")));
        for bad in ["", &ABC[1..], &ABC.to_uppercase(), &format!("{}g", &ABC[1..])] {
            assert_eq!(parse_hex(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn verify_refuses_other_bytes_and_names_both_digests() {
        assert_eq!(verify(b"abc", &digest(b"abc")), Ok(()));
        let refused = verify(b"abd", &digest(b"abc")).unwrap_err();
        assert!(refused.to_string().contains(ABC), "{refused}");
        assert!(matches!(refused, Refusal::Mismatch { .. }));
    }

    #[test]
    fn a_request_round_trips_and_a_bent_one_is_refused() {
        let request = Request { service: "netstack".into(), digest: digest(b"x"), len: 4096 };
        assert_eq!(Request::decode(&request.encode()), Ok(request.clone()));
        assert!(matches!(Request::decode(&[0; 39]), Err(Refusal::Malformed(_))));
        for service in ["", "..", "net/stack"] {
            let bent = Request { service: service.into(), ..request.clone() };
            assert!(matches!(Request::decode(&bent.encode()), Err(Refusal::Malformed(_))), "{service:?}");
        }
        let empty = Request { len: 0, ..request.clone() };
        assert!(matches!(Request::decode(&empty.encode()), Err(Refusal::Malformed(_))));
        let past = Request { len: MAX_BINARY_BYTES + 1, ..request };
        assert_eq!(Request::decode(&past.encode()), Err(Refusal::TooLarge(MAX_BINARY_BYTES + 1)));
    }

    #[test]
    fn every_word_is_spelled_once() {
        for (i, (_, a)) in WORDS.iter().enumerate() {
            for (_, b) in &WORDS[i + 1..] {
                assert!(!a.starts_with(b) && !b.starts_with(a), "{a} and {b}");
            }
        }
    }
}
