//! Replacing a running service's binary: every word sshd, `/system/bin/init`
//! and the host say about it, and every decision init makes about one. Pure.
//!
//! **A swap is asked over ssh and done by init, and nothing else can do
//! either.** sshd serves the [`SUBSYSTEM`] on an authenticated channel only; it
//! stages the bytes it was sent under [`STAGING`] and asks init over the
//! [`PORT`] init serves, which the build gate lets no program but sshd
//! receive. init is the only process holding the system capability, so it is
//! the only one that can stop a service, give its device claims back and start
//! it again holding exactly its manifest row.
//!
//! **The order is fixed and every refusal leaves the old service running.**
//! init reads the staged file, holds its bytes against the requester's
//! [`Digest`] ([`verify`]), writes them to the [`installed_path`] — a
//! temporary name and one rename, so the place a service is started from is
//! never half-written — answers [`MSG_ACCEPTED`], and waits for the requester
//! to hang up, at most [`HANGUP_MS`]. sshd hangs up once its client has closed
//! the channel the answer came on, which is that client's proof it has the
//! answer: the service being swapped may be the one carrying it, and is not
//! stopped before it has arrived. Then init stops the old
//! process, starts the new binary, and holds it on probation for
//! [`PROBATION_MS`]. A binary that does not spawn, or ends inside probation, is
//! [`Word::Failed`], and init starts the binary it replaced again.
//!
//! **A swap lasts one boot.** The root volume is the image and is read-only;
//! a replaced binary lives in the per-boot tmpfs and a reboot runs the image's
//! own. A swap that outlived the boot would be a machine booting something its
//! image does not say.

#![forbid(unsafe_code)]

use sha2::{Digest as _, Sha256};

/// The name init serves swap requests on — an `init-serve` record like
/// `launcher`.
pub const PORT: &str = "swap";

/// The endowment label [`PORT`] reaches its holder under: a namespace holding
/// that one name, and never an entry of the holder's `svc` namespace — which
/// std hands, duplicated, to every program the holder spawns directly, so a
/// name in it is a name whatever the holder runs undeclared would hold too.
pub const LABEL: &str = "swap";

/// The one program a build may let receive [`PORT`]: it serves the request
/// only on a channel whose key it has already accepted.
pub const HOLDER: &str = "sshd";

/// The SSH subsystem a client opens to ask for a swap.
pub const SUBSYSTEM: &str = "toyos-swap";

/// Where a swap's bytes are staged and where a swapped binary is started from.
pub const STAGING: &str = "/tmp/swap";

/// The largest binary a swap carries. A bound on the memory sshd and init each
/// spend on one request; a service binary in this tree is a few megabytes.
pub const MAX_BINARY_BYTES: u64 = 64 * 1024 * 1024;

/// How long a new binary must keep running before init calls it in service.
///
/// **Policy, and what "fails to start" means here**: a service in this tree has
/// no readiness message, so a binary that ends inside this window is one that
/// did not start, and one that ends after it is a service that died.
pub const PROBATION_MS: u64 = 5_000;

/// How long sshd waits for its client to close the channel it answered on —
/// the client's proof that it has the answer — before it hangs up on init
/// anyway.
pub const ANSWER_MS: u64 = 2_000;

/// How long init waits for the requester to hang up once it has answered
/// [`MSG_ACCEPTED`]. The hang-up is the go; this bounds a requester that never
/// sends one, and is wider than [`ANSWER_MS`] so a requester that waited for
/// its client is never overtaken.
pub const HANGUP_MS: u64 = 2 * ANSWER_MS;

const _: () = assert!(HANGUP_MS > ANSWER_MS);

/// The request, sshd to init: [`Request::encode`]'s bytes.
pub const MSG_SWAP: u32 = 1;
/// init's answer: verified and installed; the payload is the installed path.
pub const MSG_ACCEPTED: u32 = 2;
/// init's answer: refused, and the old service untouched; the payload is why.
pub const MSG_REFUSED: u32 = 3;

/// A SHA-256 digest.
pub type Digest = [u8; 32];

/// The digest of `bytes`: the one definition the host, sshd and init share.
pub fn digest(bytes: &[u8]) -> Digest {
    Sha256::digest(bytes).into()
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
    /// Nothing init started at boot has this name.
    NotAService(String),
    /// init started it, and it has ended: its port is closed for good.
    NotRunning(String),
    /// Another swap has not finished.
    Busy(String),
    /// The path is not one [`staged_path`] makes.
    NotStaged(String),
    Unreadable { path: String, why: String },
    TooLarge(u64),
    /// The bytes are not the ones the requester named.
    Mismatch { got: Digest, want: Digest },
    /// The service already runs this binary.
    AlreadyRuns(String),
    /// init could not put the verified bytes where a service is started from.
    Install { path: String, why: String },
    /// This sshd was given no [`PORT`]: the image does not let it swap.
    NoAuthority,
}

impl core::fmt::Display for Refusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Malformed(why) => write!(f, "the request is malformed: {why}"),
            Self::NotAService(name) => write!(f, "{name} is no service init started at boot"),
            Self::NotRunning(name) => write!(f, "{name} has ended and its port is closed"),
            Self::Busy(name) => write!(f, "a swap of {name} has not finished"),
            Self::NotStaged(path) => write!(f, "{path:?} is not a staged binary"),
            Self::Unreadable { path, why } => write!(f, "{path} cannot be read: {why}"),
            Self::TooLarge(len) => {
                write!(f, "{len} bytes is more than the {MAX_BINARY_BYTES} a swap carries")
            }
            Self::Mismatch { got, want } => {
                write!(f, "the binary hashes to {} and the request names {}", hex(got), hex(want))
            }
            Self::AlreadyRuns(path) => write!(f, "the service already runs {path}"),
            Self::Install { path, why } => write!(f, "{path} could not be written: {why}"),
            Self::NoAuthority => write!(f, "this sshd holds no `{PORT}` connector"),
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

/// What a client sends first on the [`SUBSYSTEM`] channel, one line:
/// `<service> <sha256 hex> <length>\n`, then exactly `<length>` bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub service: String,
    pub digest: Digest,
    pub len: u64,
}

/// The longest header line, newline included; a longer one is refused rather
/// than buffered.
pub const MAX_HEADER: usize = toyos_manifest::MAX_PROGRAM_NAME + 1 + 64 + 1 + 20 + 1;

impl Header {
    pub fn render(&self) -> String {
        format!("{} {} {}\n", self.service, hex(&self.digest), self.len)
    }

    /// The header at the front of `buf`, and how many bytes it took; `Ok(None)`
    /// while no newline has arrived and the line could still be one.
    pub fn take(buf: &[u8]) -> Result<Option<(Self, usize)>, Refusal> {
        let Some(end) = buf.iter().position(|&b| b == b'\n') else {
            if buf.len() >= MAX_HEADER {
                return Err(Refusal::Malformed(format!("no header in the first {MAX_HEADER} bytes")));
            }
            return Ok(None);
        };
        let line = core::str::from_utf8(&buf[..end])
            .map_err(|_| Refusal::Malformed("the header is not UTF-8".into()))?;
        let words: Vec<&str> = line.split(' ').collect();
        let [service, digest, len] = words[..] else {
            return Err(Refusal::Malformed(format!("{line:?} is not `<service> <sha256> <length>`")));
        };
        if !is_service_name(service) {
            return Err(Refusal::Malformed(format!("{service:?} is not a service name")));
        }
        let digest = parse_hex(digest)
            .ok_or_else(|| Refusal::Malformed(format!("{digest:?} is not a SHA-256 in hex")))?;
        let len: u64 =
            len.parse().map_err(|_| Refusal::Malformed(format!("{len:?} is not a length")))?;
        if len > MAX_BINARY_BYTES {
            return Err(Refusal::TooLarge(len));
        }
        Ok(Some((Self { service: service.to_string(), digest, len }, end + 1)))
    }
}

/// Where sshd stages one request's bytes. `nonce` keeps two sessions' files
/// apart; the name is never one a service is started from.
pub fn staged_path(service: &str, nonce: u64) -> String {
    format!("{STAGING}/incoming-{service}-{nonce}")
}

/// Whether `path` is one [`staged_path`] could have made.
pub fn is_staged(path: &str) -> bool {
    let Some(file) = path.strip_prefix(STAGING).and_then(|rest| rest.strip_prefix("/incoming-"))
    else {
        return false;
    };
    let Some((service, nonce)) = file.rsplit_once('-') else { return false };
    is_service_name(service) && !nonce.is_empty() && nonce.bytes().all(|b| b.is_ascii_digit())
}

/// Where a verified binary is started from: a directory named by its digest,
/// holding a file named by the service, so the process a swap starts carries
/// the service's own name wherever a name is taken from the path.
pub fn installed_path(service: &str, digest: &Digest) -> String {
    format!("{}/{service}", installed_dir(digest))
}

pub fn installed_dir(digest: &Digest) -> String {
    format!("{STAGING}/{}", hex(digest))
}

/// Whether `path` is a binary a swap installed, which is what init may delete
/// once nothing runs it. A path in the image never is.
pub fn is_installed(path: &str) -> bool {
    installed_digest(path).is_some()
}

/// The digest an installed binary's path names, which its bytes must still
/// hash to when it is started; `None` for any other path.
pub fn installed_digest(path: &str) -> Option<Digest> {
    let rest = path.strip_prefix(STAGING)?.strip_prefix('/')?;
    let (dir, service) = rest.split_once('/')?;
    if !is_service_name(service) {
        return None;
    }
    parse_hex(dir)
}

/// The request sshd sends init: the service, the staged path and the digest the
/// client named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub service: String,
    pub staged: String,
    pub digest: Digest,
}

impl Request {
    /// `digest ‖ service ‖ 0 ‖ staged`.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(32 + self.service.len() + 1 + self.staged.len());
        out.extend_from_slice(&self.digest);
        out.extend_from_slice(self.service.as_bytes());
        out.push(0);
        out.extend_from_slice(self.staged.as_bytes());
        out
    }

    /// Everything in it is the requester's claim: a name that is not a service
    /// name and a path that is not a staged one are refused here, before init
    /// looks anything up or opens anything.
    pub fn decode(bytes: &[u8]) -> Result<Self, Refusal> {
        if bytes.len() < 32 {
            return Err(Refusal::Malformed(format!("{} bytes is shorter than a digest", bytes.len())));
        }
        let (digest, rest) = bytes.split_at(32);
        let digest: Digest = digest.try_into().expect("split at 32");
        let text = core::str::from_utf8(rest)
            .map_err(|_| Refusal::Malformed("the names are not UTF-8".into()))?;
        let (service, staged) = text
            .split_once('\0')
            .ok_or_else(|| Refusal::Malformed("no separator between the service and the path".into()))?;
        if !is_service_name(service) {
            return Err(Refusal::Malformed(format!("{service:?} is not a service name")));
        }
        if !is_staged(staged) {
            return Err(Refusal::NotStaged(staged.to_string()));
        }
        Ok(Self { service: service.to_string(), staged: staged.to_string(), digest })
    }
}

/// What init says about a swap, in the one line form every reader matches:
/// `init: swap <service>: <word>: <detail>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Word {
    Refused,
    /// The bytes matched and are installed; the requester has been answered.
    Accepted,
    /// The old process is being stopped.
    Stopping,
    /// The new binary is running and on probation.
    Started,
    /// Probation passed: the new binary is the service.
    InService,
    /// The new binary did not spawn or ended on probation.
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

    /// Whether this word ends a swap: nothing more is said about it after.
    pub fn is_final(self) -> bool {
        matches!(self, Self::Refused | Self::InService | Self::Restored | Self::Gone)
    }
}

pub fn said(service: &str, word: Word, detail: &str) -> String {
    format!("init: swap {service}: {}: {detail}", word.as_str())
}

/// init's line about `service` inside `line` — a console line or a record in
/// any of the forms the log renders one in — as `(word, detail)`.
pub fn heard<'a>(line: &'a str, service: &str) -> Option<(Word, &'a str)> {
    let head = format!("init: swap {service}: ");
    let (_, rest) = line.split_once(&head)?;
    let rest = rest.trim_end_matches(['\n', '\r']);
    WORDS.iter().find_map(|(word, spelled)| {
        rest.strip_prefix(spelled).and_then(|r| r.strip_prefix(": ")).map(|detail| (*word, detail))
    })
}

/// What a swap came to, read off init's lines about it: the last final word
/// among `lines`, or `None` while the swap is still going.
pub fn outcome<'a>(lines: impl IntoIterator<Item = &'a str>, service: &str) -> Option<(Word, String)> {
    lines
        .into_iter()
        .filter_map(|line| heard(line, service))
        .filter(|(word, _)| word.is_final())
        .last()
        .map(|(word, detail)| (word, detail.to_string()))
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
    fn a_header_round_trips_and_waits_for_its_newline() {
        let header = Header { service: "netd".into(), digest: digest(b"abc"), len: 2_317_912 };
        let line = header.render();
        let mut buf = line.clone().into_bytes();
        buf.extend_from_slice(b"\x7fELF");
        assert_eq!(Header::take(&buf), Ok(Some((header, line.len()))));
        assert_eq!(Header::take(&line.as_bytes()[..10]), Ok(None));
        assert!(line.len() <= MAX_HEADER);
    }

    #[test]
    fn a_header_that_is_not_one_is_refused() {
        let ok_digest = ABC;
        for line in [
            format!("netd {ok_digest}\n"),
            format!("netd {ok_digest} 12 extra\n"),
            format!("../netd {ok_digest} 12\n"),
            format!("netd {} 12\n", &ok_digest[1..]),
            format!("netd {ok_digest} twelve\n"),
            format!("netd {ok_digest} {}\n", MAX_BINARY_BYTES + 1),
        ] {
            assert!(Header::take(line.as_bytes()).is_err(), "{line:?}");
        }
        assert!(Header::take(&[b'x'; MAX_HEADER]).is_err());
    }

    #[test]
    fn a_request_round_trips_and_a_path_it_did_not_stage_is_refused() {
        let request =
            Request { service: "netd".into(), staged: staged_path("netd", 7), digest: digest(b"x") };
        assert_eq!(Request::decode(&request.encode()), Ok(request.clone()));
        for staged in [
            "/system/bin/netd",
            "/tmp/swap/incoming-netd-",
            "/tmp/swap/incoming-netd-7/../../../system/bin/sh",
            "/tmp/swap/incoming-../x-7",
            "/tmp/swap/incoming-netd-7x",
            "/tmp/swapincoming-netd-7",
        ] {
            let bent = Request { staged: staged.into(), ..request.clone() };
            assert_eq!(
                Request::decode(&bent.encode()),
                Err(Refusal::NotStaged(staged.into())),
                "{staged}"
            );
        }
        assert!(Request::decode(&[0; 31]).is_err());
        let nameless = Request { service: "".into(), ..request };
        assert!(matches!(Request::decode(&nameless.encode()), Err(Refusal::Malformed(_))));
    }

    /// The process a swap starts is named after its path's last component, so
    /// the installed file carries the service's own name and nothing else.
    #[test]
    fn an_installed_binary_is_named_by_its_service_under_its_digest() {
        let path = installed_path("netd", &digest(b"abc"));
        assert_eq!(path, format!("/tmp/swap/{ABC}/netd"));
        assert!(is_installed(&path));
        assert_eq!(installed_digest(&path), Some(digest(b"abc")));
        assert!(!is_installed("/system/bin/netd"));
        assert!(!is_installed(&staged_path("netd", 1)));
        assert!(!is_staged(&path));
    }

    #[test]
    fn inits_lines_are_heard_in_every_form_the_log_renders_them() {
        let line = said("netd", Word::InService, "/tmp/swap/x/netd as pid 9");
        assert_eq!(line, "init: swap netd: in service: /tmp/swap/x/netd as pid 9");
        for rendered in [
            format!("{line}\n"),
            format!("[2026-09-23 18:00:01 12.345 cpu1] @{line}\n"),
        ] {
            assert_eq!(
                heard(&rendered, "netd"),
                Some((Word::InService, "/tmp/swap/x/netd as pid 9")),
                "{rendered:?}"
            );
        }
        assert_eq!(heard(&line, "sshd"), None);
        assert_eq!(heard("init: swap netd: bored: x", "netd"), None);
    }

    #[test]
    fn the_outcome_is_the_last_final_word() {
        let lines = [
            said("netd", Word::Accepted, "a"),
            said("netd", Word::Stopping, "b"),
            said("netd", Word::Started, "c"),
        ];
        assert_eq!(outcome(lines.iter().map(String::as_str), "netd"), None);
        let mut more = lines.to_vec();
        more.push(said("netd", Word::Failed, "d"));
        more.push(said("netd", Word::Restored, "/system/bin/netd as pid 4"));
        assert_eq!(
            outcome(more.iter().map(String::as_str), "netd"),
            Some((Word::Restored, "/system/bin/netd as pid 4".to_string()))
        );
        for word in [Word::Accepted, Word::Stopping, Word::Started, Word::Failed] {
            assert!(!word.is_final());
        }
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
