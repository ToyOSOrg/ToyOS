//! The `authorized_keys` file (sshd(8), AUTHORIZED_KEYS FILE FORMAT): one key
//! a line, `[options] keytype base64 [comment]`, blank lines and `#` lines
//! ignored.
//!
//! **An entry carrying options authorizes nothing.** Options restrict what a
//! key may do, none is implemented here, and honouring the key while dropping
//! its restrictions would grant more than the file says. A line whose first
//! field is not a key type, and a later one is, is an options entry.

use crate::base64;
use crate::hostkey::{self, public_blob};

/// The longest line read: sshd(8)'s own limit.
const LINE_CAP: usize = 8 * 1024;

/// The key types sshd(8) names. A line starting with one has no options.
const KEY_TYPES: [&str; 7] = [
    "sk-ecdsa-sha2-nistp256@openssh.com",
    "ecdsa-sha2-nistp256",
    "ecdsa-sha2-nistp384",
    "ecdsa-sha2-nistp521",
    "sk-ssh-ed25519@openssh.com",
    "ssh-ed25519",
    "ssh-rsa",
];

/// One line of the file that is not blank or a comment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    /// An `ssh-ed25519` key with no options: one that authorizes.
    Key([u8; 32]),
    /// A key with options, which authorizes nothing.
    Restricted,
    /// A key of a type this server does not take.
    OtherType(String),
    /// A line that is not a key, and why.
    Unreadable(&'static str),
}

/// Every entry of `text`, in order.
pub fn entries(text: &str) -> impl Iterator<Item = Entry> + '_ {
    text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')).map(entry)
}

/// Does `text` name `key` in an entry that authorizes?
pub fn authorizes(text: &str, key: &[u8; 32]) -> bool {
    entries(text).any(|entry| entry == Entry::Key(*key))
}

fn entry(line: &str) -> Entry {
    if line.len() > LINE_CAP {
        return Entry::Unreadable("a line longer than 8 KiB");
    }
    let mut fields = line.split_ascii_whitespace();
    let kind = fields.next().expect("a line that is not blank has a field");
    if !KEY_TYPES.contains(&kind) {
        return match fields.any(|field| KEY_TYPES.contains(&field)) {
            true => Entry::Restricted,
            false => Entry::Unreadable("a line naming no key type"),
        };
    }
    if kind != hostkey::ALGORITHM {
        return Entry::OtherType(kind.to_string());
    }
    let Some(blob) = fields.next() else { return Entry::Unreadable("a key type with no key") };
    let Ok(blob) = base64::decode(blob) else { return Entry::Unreadable("a key that is not base64") };
    match blob.get(blob.len().saturating_sub(32)..).and_then(|key| <[u8; 32]>::try_from(key).ok()) {
        Some(key) if public_blob(&key) == blob => Entry::Key(key),
        _ => Entry::Unreadable("an ssh-ed25519 line whose blob is not one Ed25519 key"),
    }
}

/// `ssh-keygen`'s own lines, so what is under test is the reading and not a
/// re-encoding of it.
#[cfg(test)]
mod tests {
    use super::*;

    const USER: &str = include_str!("../tests/fixtures/user_ed25519.pub");
    const STRANGER: &str = include_str!("../tests/fixtures/stranger_ed25519.pub");
    const RSA: &str = include_str!("../tests/fixtures/user_rsa.pub");

    fn key(line: &str) -> [u8; 32] {
        let Entry::Key(key) = entry(line.trim()) else { panic!("{line} is a key") };
        key
    }

    #[test]
    fn a_listed_key_authorizes_and_no_other() {
        assert!(authorizes(USER, &key(USER)));
        assert!(!authorizes(USER, &key(STRANGER)));
        assert_ne!(key(USER), key(STRANGER));
    }

    #[test]
    fn a_file_that_names_nobody_authorizes_nobody() {
        for text in ["", "\n", "   \n\t\n", "# a comment\n", "garbage\n", RSA] {
            assert!(!authorizes(text, &key(USER)), "{text:?} authorized a key");
        }
    }

    /// The load-bearing refusal: the key is listed, but under restrictions
    /// this server does not implement.
    #[test]
    fn a_key_listed_with_options_authorizes_nothing() {
        for options in ["command=\"/system/bin/shell -c ls\"", "no-pty", "restrict", "from=\"10.0.0.1\",no-agent-forwarding"] {
            let text = format!("{options} {USER}");
            assert!(!authorizes(&text, &key(USER)), "{options:?} let the key through");
            assert_eq!(entries(&text).collect::<Vec<_>>(), [Entry::Restricted]);
        }
    }

    #[test]
    fn the_comment_is_not_part_of_the_key() {
        let line = USER.rsplit_once(' ').unwrap().0;
        assert!(authorizes(&format!("{line} somebody@another-machine\n"), &key(USER)));
        assert!(authorizes(line, &key(USER)));
    }

    /// A line that is not a key does not disarm the keys around it, and is
    /// named for what it is.
    #[test]
    fn every_line_is_read_on_its_own() {
        let text = format!("# keys\n{STRANGER}\nnot a key at all\n\n{RSA}ssh-ed25519 AAAA\nssh-ed25519\n{USER}");
        assert!(authorizes(&text, &key(USER)));
        assert!(authorizes(&text, &key(STRANGER)));
        assert_eq!(
            entries(&text).collect::<Vec<_>>(),
            [
                Entry::Key(key(STRANGER)),
                Entry::Unreadable("a line naming no key type"),
                Entry::OtherType("ssh-rsa".into()),
                Entry::Unreadable("an ssh-ed25519 line whose blob is not one Ed25519 key"),
                Entry::Unreadable("a key type with no key"),
                Entry::Key(key(USER)),
            ]
        );
    }

    /// The blob names its own algorithm: an RSA blob under `ssh-ed25519`, or a
    /// line past sshd(8)'s 8 KiB, is no key.
    #[test]
    fn a_blob_that_is_not_its_lines_key_is_refused() {
        let rsa_blob = RSA.split(' ').nth(1).unwrap();
        assert!(matches!(entry(&format!("ssh-ed25519 {rsa_blob}")), Entry::Unreadable(_)));
        assert_eq!(entry(&format!("{} {}", USER.trim(), "x".repeat(LINE_CAP))), Entry::Unreadable("a line longer than 8 KiB"));
    }
}
