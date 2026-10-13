//! `toyos-root`: the package repository's root-key ceremony, run on a machine
//! that has no network and will never have one, and the only ToyOS program
//! that machine runs.
//!
//! ```text
//! toyos-root new <key file>
//! toyos-root sign --key <key file> --signing <public line file> --out <root.1.txt>
//! ```
//!
//! `new` mints the root key into a file that must not exist yet: its seed in
//! 64 hex digits and a newline, as the build's other key files hold one. It
//! prints the key's fingerprint and public line. `sign` writes root 1
//! (`toyos_update::repo::render::first_root`): the root key alone holds the
//! root role and the signing key, named by the public line
//! `cargo run -- --repo-signing-key-new` printed, the timestamp and targets
//! roles, each at threshold 1, expiring a year from the machine's clock. It
//! reads root 1 back as self-signed before it writes it, and prints it with
//! both fingerprints. The key file's mode is not checked: what guards it is
//! the stick's custody, and a check would bind the tool to one kind of
//! filesystem and host.
//!
//! **The ceremony.** The owner brings a machine that is offline for good, two
//! USB sticks A and B for the root key, formatted on that machine, and a third,
//! C, that carries files between it and the online machine.
//!
//! 1. Online, before.
//!    1. `cargo run -q -- --repo-signing-key-new > <C>/signing.pub` mints the
//!       signing key at `~/.config/toyos/repo-signing-key` (`0600`), prints
//!       its fingerprint, and writes its public line to the stick. Write the
//!       fingerprint on paper.
//!    2. `cargo build --release -p toyos-root --target <the offline machine's
//!       triple>`, and copy `target/<triple>/release/toyos-root` to C.
//! 2. Offline.
//!    1. `date -u`: the clock must say today, since root 1 expires a year from
//!       it.
//!    2. `toyos-root new <A>/root-key`. Write its fingerprint on the paper.
//!    3. `cp <A>/root-key <B>/root-key && cmp <A>/root-key <B>/root-key`.
//!    4. `toyos-root sign --key <A>/root-key --signing <C>/signing.pub --out
//!       <C>/root.1.txt`. Both fingerprints it prints must be the paper's,
//!       and its expiry a year from today.
//!    5. Unmount and unplug A and B, and keep them apart, offline, each in a
//!       place of its own. Only `<C>/root.1.txt` goes back.
//! 3. Online, after.
//!    1. `root.1.txt` is committed as `roots/root.1.txt` in `ToyOSOrg/packages`
//!       and as `images/pkg/root.1.txt` in ToyOS, each read back through
//!       `toyos_update::repo::self_signed` and the publisher.
//!    2. The signing key file's contents become the `REPO_SIGNING_KEY` secret
//!       of `packages`'s `publish` environment, and the file is deleted.
//!
//! Signing root N+1 from root N, renewal and rotation are not built: a
//! machine holding root 1 is held to its keys until they are.

use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use toyos_update::repo::{self, PublicKey};

/// How long root 1 is good for.
const LIFE: u64 = 365 * 86_400;

const USAGE: &str = "usage: toyos-root new <key file>\n       \
                     toyos-root sign --key <key file> --signing <public line file> --out <root.1.txt>";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let said = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["new", key] => new(Path::new(key)),
        ["sign", "--key", key, "--signing", signing, "--out", out] => {
            let now = SystemTime::now().duration_since(UNIX_EPOCH).expect("a clock after 1970").as_secs();
            sign(Path::new(key), Path::new(signing), Path::new(out), now)
        }
        _ => Err(USAGE.into()),
    };
    match said {
        Ok(text) => print!("{text}"),
        Err(why) => {
            eprintln!("toyos-root: {why}");
            std::process::exit(1);
        }
    }
}

/// Mint the root key into `path`, which must not exist.
fn new(path: &Path) -> Result<String, String> {
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).map_err(|e| format!("the operating system's randomness: {e}"))?;
    let hex: String = seed.iter().map(|b| format!("{b:02x}")).collect();
    write_new(path, format!("{hex}\n").as_bytes())?;
    let public = toyos_update::sig::public_of(&seed);
    Ok(format!("root key {}\n{}\n", repo::fingerprint(&public), repo::public_line(&public)))
}

/// Sign root 1 with the root key at `key`, granting the key `signing` names,
/// into `out`, which must not exist.
fn sign(key: &Path, signing: &Path, out: &Path, now: u64) -> Result<String, String> {
    let text = std::fs::read_to_string(key).map_err(|e| format!("{}: {e}", key.display()))?;
    let seed = seed_of_hex(text.strip_suffix('\n').unwrap_or(&text))
        .ok_or_else(|| format!("{} is not 64 hex digits, which `toyos-root new` writes", key.display()))?;
    let line = std::fs::read_to_string(signing).map_err(|e| format!("{}: {e}", signing.display()))?;
    let signing_key: PublicKey = repo::parse_public_line(line.strip_suffix('\n').unwrap_or(&line))
        .ok_or_else(|| format!("{} is not one `ssh-ed25519 <base64>` line", signing.display()))?;
    let root_key = toyos_update::sig::public_of(&seed);
    if root_key == signing_key {
        return Err(format!("{} names the root key, which holds the root role alone", signing.display()));
    }
    let bytes = repo::render::first_root(&seed, &signing_key, now + LIFE);
    let root = repo::self_signed(&bytes).expect("the root this signed is self-signed");
    write_new(out, &bytes)?;
    Ok(format!(
        "{}root key {}\nsigning key {}\nexpires {}\n",
        String::from_utf8(bytes).expect("a root is ASCII"),
        repo::fingerprint(&root_key),
        repo::fingerprint(&signing_key),
        repo::time_text(root.expires)
    ))
}

/// `bytes` written to `path` and synced, where no file is there.
fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => format!("{} already exists, and it is not replaced", path.display()),
        _ => format!("{}: {e}", path.display()),
    })?;
    file.write_all(bytes).and_then(|()| file.sync_all()).map_err(|e| format!("{}: {e}", path.display()))
}

fn seed_of_hex(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 || !text.is_ascii() {
        return None;
    }
    let mut seed = [0u8; 32];
    for (i, byte) in seed.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(seed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use repo::{render, Held, Item, Mirror, Role, Snapshot, Targets, Timestamp};
    use toyos_tmpdir::TempDir;

    /// 2026-10-09T00:00:00Z.
    const NOW: u64 = 1_791_504_000;
    const SIGNING: [u8; 32] = [3; 32];

    struct Repo(Vec<(String, Vec<u8>)>);

    impl Mirror for Repo {
        fn fetch(&mut self, name: &str, cap: usize) -> Result<Option<Vec<u8>>, String> {
            Ok(self.0.iter().find(|(n, _)| n == name).map(|(_, b)| b[..b.len().min(cap + 1)].to_vec()))
        }
    }

    /// A targets and the timestamp naming it, both signed by `seed`.
    fn repository(seed: &[u8; 32]) -> Repo {
        let sha256 = toyos_update::sha256(b"archive");
        let item = Item {
            name: "gbae".into(),
            target: "x86_64-unknown-toyos".into(),
            sequence: 1,
            version: "1.0".into(),
            url: "gbae-1-x86_64-unknown-toyos.tar.gz".into(),
            length: 7,
            sha256,
        };
        let body = render::targets(&Targets { version: 1, expires: NOW + 86_400, items: vec![item] });
        let targets = (body.clone() + &render::signature(seed, Role::Targets, body.as_bytes())).into_bytes();
        let snapshot = Snapshot { version: 1, length: targets.len() as u64, sha256: toyos_update::sha256(&targets) };
        let body = render::timestamp(&Timestamp { version: 1, expires: NOW + 86_400, targets: snapshot });
        let timestamp = (body.clone() + &render::signature(seed, Role::Timestamp, body.as_bytes())).into_bytes();
        Repo(vec![("targets.1.txt".into(), targets), ("timestamp.txt".into(), timestamp)])
    }

    /// **The ceremony's output is a root 1 a machine can pin**: minted and
    /// signed through the two commands, it reads back self-signed under its
    /// root key, and a repository the signing key signs is one a machine
    /// pinning it accepts, where one the root key signs is not.
    #[test]
    fn a_root_one_it_signs_is_self_signed_and_grants_the_signing_key() {
        let dir = TempDir::new("toyos-root");
        let (key, signing, out) = (dir.path().join("root-key"), dir.path().join("signing.pub"), dir.path().join("root.1.txt"));
        let said = new(&key).expect("minted");
        assert!(new(&key).unwrap_err().ends_with("already exists, and it is not replaced"));
        std::fs::write(&signing, format!("{}\n", repo::public_line(&toyos_update::sig::public_of(&SIGNING)))).unwrap();
        let printed = sign(&key, &signing, &out, NOW).expect("signed");
        let bytes = std::fs::read(&out).unwrap();
        let root = repo::self_signed(&bytes).expect("self-signed");
        assert_eq!((root.version, root.expires), (1, NOW + LIFE));
        let root_fingerprint = said.lines().next().unwrap().strip_prefix("root key ").unwrap();
        assert!(printed.contains(&format!("root key {root_fingerprint}\n")), "{printed}");
        assert!(sign(&key, &signing, &out, NOW).unwrap_err().ends_with("already exists, and it is not replaced"));

        let image = Held { root: &bytes, timestamp: None, targets: None };
        repo::refresh(&mut repository(&SIGNING), image, None, NOW).expect("what the signing key signs");
        let seed = seed_of_hex(std::fs::read_to_string(&key).unwrap().trim_end()).unwrap();
        assert!(repo::refresh(&mut repository(&seed), image, None, NOW).is_err(), "the root key signs roots alone");
    }

    /// A signing line that is not one Ed25519 public line, or that names the
    /// root key itself, is refused, and no root is written.
    #[test]
    fn a_signing_line_that_is_not_another_key_is_refused() {
        let dir = TempDir::new("toyos-root");
        let (key, signing, out) = (dir.path().join("root-key"), dir.path().join("signing.pub"), dir.path().join("root.1.txt"));
        let said = new(&key).unwrap();
        std::fs::write(&signing, said.lines().nth(1).unwrap()).unwrap();
        assert!(sign(&key, &signing, &out, NOW).unwrap_err().ends_with("names the root key, which holds the root role alone"));
        std::fs::write(&signing, "ssh-ed25519 AAAA comment\n").unwrap();
        assert!(sign(&key, &signing, &out, NOW).unwrap_err().ends_with("is not one `ssh-ed25519 <base64>` line"));
        assert!(!out.exists(), "a refused sign wrote no root");
    }
}
