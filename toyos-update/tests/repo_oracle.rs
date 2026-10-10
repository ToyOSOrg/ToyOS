//! **The independent oracle for the repository's signatures**: a repository
//! whose every signature OpenSSH made is one the client accepts, and one byte
//! bent anywhere in it is refused.
//!
//! The fixture is OpenSSH's own: `ssh-keygen -t ed25519` minted a throwaway
//! key, the three bodies were written by hand to the format, `ssh-keygen -Y
//! sign -n toyos-<role>` signed each, its armour's base64 became the `sig`
//! line, and the private key was deleted. The key ID in each was taken from
//! `ssh-keygen -l`'s SHA-256 fingerprint, so the client agreeing with it is
//! the key ID's definition checked too. Nothing runs `ssh-keygen` at test time.

use toyos_update::repo::{self, Archive, Held, Mirror, Refused, Role, SigRefused};

const ROOT: &[u8] = include_bytes!("fixtures/repo/root.1.txt");
const TIMESTAMP: &[u8] = include_bytes!("fixtures/repo/timestamp.txt");
const TARGETS: &[u8] = include_bytes!("fixtures/repo/targets.1.txt");
/// The archive the targets names, by length and SHA-256.
const ARCHIVE: &[u8] = b"the archive ssh-keygen vouched for\n";
/// 2026-10-09T00:00:00Z, inside every expiry the fixture carries.
const NOW: u64 = 1_791_504_000;

struct Fixture {
    timestamp: Vec<u8>,
    targets: Vec<u8>,
}

impl Mirror for Fixture {
    fn fetch(&mut self, name: &str, cap: usize) -> Result<Option<Vec<u8>>, String> {
        let bytes = match name {
            "timestamp.txt" => &self.timestamp,
            "targets.1.txt" => &self.targets,
            _ => return Ok(None),
        };
        Ok(Some(bytes[..bytes.len().min(cap + 1)].to_vec()))
    }
}

fn refresh(root: &[u8], timestamp: &[u8], targets: &[u8]) -> Result<repo::Fresh, Refused> {
    let mut mirror = Fixture { timestamp: timestamp.to_vec(), targets: targets.to_vec() };
    repo::refresh(&mut mirror, Held { root, timestamp: None, targets: None }, None, NOW)
}

/// The refusal, and never a print of what was accepted.
fn refused(result: Result<repo::Fresh, Refused>) -> Refused {
    match result {
        Ok(_) => panic!("the client accepted a bent repository"),
        Err(why) => why,
    }
}

#[test]
fn a_repository_ssh_keygen_signed_is_accepted_and_a_bent_one_is_not() {
    let fresh = refresh(ROOT, TIMESTAMP, TARGETS).expect("the repository OpenSSH signed");
    let hello = fresh.targets.items.iter().find(|i| (i.name.as_str(), i.target.as_str()) == ("hello", "x86_64-unknown-toyos")).expect("the item");
    let mut archive = Archive::of(hello);
    archive.take(ARCHIVE).expect("within its length");
    archive.finish().expect("its SHA-256");

    // The low bit of one character flipped, so the document stays the format
    // and only what it says changes: a day of the timestamp's expiry, a
    // character of its signature, the targets' sequence. The root is pinned,
    // so its own bend is refused as the walk's next root.
    let bend = |doc: &[u8], at: usize| {
        let mut bent = doc.to_vec();
        bent[at] ^= 1;
        bent
    };
    let at = |doc: &[u8], text: &str| doc.windows(text.len()).position(|w| w == text.as_bytes()).unwrap() + text.len() - 1;

    let timestamp = refused(refresh(ROOT, &bend(TIMESTAMP, at(TIMESTAMP, "expires 2026-10-16")), TARGETS));
    assert!(
        matches!(timestamp, Refused::Threshold { role: Role::Timestamp, first: Some(SigRefused::Signature), .. }),
        "{timestamp}"
    );
    let signature = refused(refresh(ROOT, &bend(TIMESTAMP, TIMESTAMP.len() - 10), TARGETS));
    assert!(matches!(signature, Refused::Threshold { role: Role::Timestamp, .. }), "{signature}");

    // A targets bent is no longer the SHA-256 the timestamp names.
    let targets = refused(refresh(ROOT, TIMESTAMP, &bend(TARGETS, at(TARGETS, "sequence 1"))));
    assert_eq!(targets, Refused::TargetsDigest { version: 1 });

    struct Next(Vec<u8>);
    impl Mirror for Next {
        fn fetch(&mut self, name: &str, cap: usize) -> Result<Option<Vec<u8>>, String> {
            Ok((name == "root.2.txt").then(|| self.0[..self.0.len().min(cap + 1)].to_vec()))
        }
    }
    let as_two = String::from_utf8(ROOT.to_vec()).unwrap().replacen("toyos-repo root 1", "toyos-repo root 2", 1);
    let walked = refused(repo::refresh(
        &mut Next(as_two.into_bytes()),
        Held { root: ROOT, timestamp: None, targets: None },
        None,
        NOW,
    ));
    assert!(
        matches!(walked, Refused::Threshold { role: Role::Root, version: 2, root: 1, first: Some(SigRefused::Signature), .. }),
        "{walked}"
    );
}
