//! **The independent oracle for the image signature**: a header signed by an
//! implementation this tree did not write verifies under the loader's and the
//! updater's verifier, and the same signature over one bent byte does not.
//!
//! The fixture is OpenSSH's own: `ssh-keygen -t ed25519` minted a throwaway
//! key, `ssh-keygen -Y sign -n toyos-image` signed [`oracle_header`]'s bytes,
//! and the private key was deleted — the signature and the public key are
//! committed, no secret is. The header is rebuilt here from the same inputs, so
//! the fixture is a signature over bytes this crate writes, not over a file it
//! only carries. Nothing runs `ssh-keygen` at test time.

use toyos_update::image::{Header, HEADER_BYTES};

const SIGNATURE: &str = include_str!("fixtures/ssh-keygen-signed-header.sig");
const PUBLIC: &str = include_str!("fixtures/ssh-keygen-signer.pub");

/// What was signed: fixed inputs, so this crate writes the same bytes again.
fn oracle_header() -> [u8; HEADER_BYTES] {
    Header::of(
        20260926,
        b"\x7fELF oracle kernel",
        b"root=00000000-0000-0000-0000-000000000000",
        &[0x5A; 4096],
    )
    .encode()
}

/// RFC 4648 base64, the armour's alphabet; test-only, so no crate for it.
fn base64(text: &str) -> Vec<u8> {
    let value = |c: u8| -> u32 {
        match c {
            b'A'..=b'Z' => u32::from(c - b'A'),
            b'a'..=b'z' => u32::from(c - b'a') + 26,
            b'0'..=b'9' => u32::from(c - b'0') + 52,
            b'+' => 62,
            b'/' => 63,
            _ => panic!("{c:#x} is not base64"),
        }
    };
    let digits: Vec<u8> = text.bytes().filter(|c| !c.is_ascii_whitespace() && *c != b'=').collect();
    let mut out = Vec::new();
    for chunk in digits.chunks(4) {
        let n = chunk.iter().enumerate().fold(0u32, |acc, (i, &c)| acc | value(c) << (18 - 6 * i));
        out.extend_from_slice(&n.to_be_bytes()[1..chunk.len()]);
    }
    out
}

/// One SSH wire `string` off the front of `bytes`.
fn string<'a>(bytes: &mut &'a [u8]) -> &'a [u8] {
    let len = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
    let (s, rest) = bytes[4..].split_at(len);
    *bytes = rest;
    s
}

/// `(public key, signature)` out of an SSHSIG blob, each field held to
/// PROTOCOL.sshsig on the way.
fn sshsig(blob: &[u8]) -> ([u8; 32], [u8; 64]) {
    assert_eq!(&blob[..6], b"SSHSIG");
    assert_eq!(&blob[6..10], &1u32.to_be_bytes(), "SSHSIG version 1");
    let mut rest = &blob[10..];
    let mut public = string(&mut rest);
    assert_eq!(string(&mut public), b"ssh-ed25519");
    let public: [u8; 32] = string(&mut public).try_into().unwrap();
    assert_eq!(string(&mut rest), toyos_update::sig::NAMESPACE.as_bytes());
    assert_eq!(string(&mut rest), b"", "the reserved field");
    assert_eq!(string(&mut rest), b"sha512");
    let mut signature = string(&mut rest);
    assert_eq!(string(&mut signature), b"ssh-ed25519");
    let signature: [u8; 64] = string(&mut signature).try_into().unwrap();
    assert!(rest.is_empty());
    (public, signature)
}

#[test]
fn a_header_ssh_keygen_signed_verifies_and_a_bent_one_does_not() {
    let armour: String = SIGNATURE
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .collect();
    let (public, signature) = sshsig(&base64(&armour));

    // The key the blob names is the one the `.pub` line carries.
    let mut words = PUBLIC.split_whitespace();
    assert_eq!(words.next(), Some("ssh-ed25519"));
    let mut blob = &base64(words.next().unwrap())[..];
    assert_eq!(string(&mut blob), b"ssh-ed25519");
    assert_eq!(string(&mut blob), public);

    let header = oracle_header();
    assert_eq!(toyos_update::sig::verify(&public, &header, &signature), Ok(()));

    for at in [0, 16, 100, HEADER_BYTES - 1] {
        let mut bent = header;
        bent[at] ^= 1;
        assert_eq!(
            toyos_update::sig::verify(&public, &bent, &signature),
            Err(toyos_update::sig::Refused::Signature),
            "a header bent at byte {at}"
        );
    }
    let mut other = public;
    other[0] ^= 1;
    assert!(toyos_update::sig::verify(&other, &header, &signature).is_err(), "another key");
}
