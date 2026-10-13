//! `toyos-root`: the package repository's root-key ceremony, run on a machine
//! that has no network and will never have one, and the only ToyOS program
//! that machine runs.
//!
//! ```text
//! toyos-root new <key file>
//! toyos-root sign --key <key file> --signing <public line file> --out <root.1.txt>
//! ```
//!
//! `new` mints the root key, `0600`, into a file that must not exist yet: its
//! seed as `toyos_update::sig::seed_text` writes one. It prints the key's
//! fingerprint and public line. `sign` writes root 1
//! (`toyos_update::repo::render::first_root`): the root key alone holds the
//! root role and the signing key, named by the public line
//! `cargo run -- --repo-signing-key-new` printed, the timestamp and targets
//! roles, each at threshold 1, expiring a year from the machine's clock. It
//! reads root 1 back as self-signed before it writes it, and prints it with
//! both fingerprints. It builds for a Unix triple, which mints the key
//! `0600`.
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
//!    2. `cargo build --release -p toyos-root --target aarch64-apple-darwin`
//!       on this Mac, for an offline Mac of the same macOS and architecture,
//!       and copy `target/aarch64-apple-darwin/release/toyos-root` to C. That
//!       path is the measured one; a build for any other offline triple is
//!       unmeasured.
//! 2. Offline.
//!    1. `date -u`: the clock must say today, since root 1 expires a year from
//!       it.
//!    2. `toyos-root new <A>/root-key` mints the root key `0600`. Write its
//!       fingerprint on the paper.
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
use std::os::unix::fs::OpenOptionsExt;
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
    write_new(path, toyos_update::sig::seed_text(&seed).as_bytes(), 0o600)?;
    let public = toyos_update::sig::public_of(&seed);
    Ok(format!("root key {}\n{}\n", repo::fingerprint(&public), repo::public_line(&public)))
}

/// Sign root 1 with the root key at `key`, granting the key `signing` names,
/// into `out`, which must not exist.
fn sign(key: &Path, signing: &Path, out: &Path, now: u64) -> Result<String, String> {
    let text = std::fs::read_to_string(key).map_err(|e| format!("{}: {e}", key.display()))?;
    let seed = toyos_update::sig::seed_of_text(&text)
        .ok_or_else(|| format!("{} is not the 64 hex digits and newline `toyos-root new` writes", key.display()))?;
    let line = std::fs::read_to_string(signing).map_err(|e| format!("{}: {e}", signing.display()))?;
    let signing_key: PublicKey = repo::parse_public_line(line.strip_suffix('\n').unwrap_or(&line))
        .ok_or_else(|| format!("{} is not one `ssh-ed25519 <base64>` line", signing.display()))?;
    let root_key = toyos_update::sig::public_of(&seed);
    if root_key == signing_key {
        return Err(format!("{} names the root key, which holds the root role alone", signing.display()));
    }
    let bytes = repo::render::first_root(&seed, &signing_key, now + LIFE);
    let root = repo::self_signed(&bytes).expect("the root this signed is self-signed");
    write_new(out, &bytes, 0o644)?;
    Ok(format!(
        "{}root key {}\nsigning key {}\nexpires {}\n",
        String::from_utf8(bytes).expect("a root is ASCII"),
        repo::fingerprint(&root_key),
        repo::fingerprint(&signing_key),
        repo::time_text(root.expires)
    ))
}

/// `bytes` written to `path` at `mode` and synced, where no file is there.
fn write_new(path: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(mode).open(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => format!("{} already exists, and it is not replaced", path.display()),
        _ => format!("{}: {e}", path.display()),
    })?;
    file.write_all(bytes).and_then(|()| file.sync_all()).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use toyos_tmpdir::TempDir;

    /// 2026-10-09T00:00:00Z.
    const NOW: u64 = 1_791_504_000;
    const SIGNING: [u8; 32] = [3; 32];

    /// A minted root key, a signing line naming [`SIGNING`], and where root 1
    /// goes, with what `new` printed.
    struct Ceremony {
        _dir: TempDir,
        key: std::path::PathBuf,
        signing: std::path::PathBuf,
        out: std::path::PathBuf,
        said: String,
    }

    fn ceremony() -> Ceremony {
        let dir = TempDir::new("toyos-root");
        let (key, signing, out) = (dir.path().join("root-key"), dir.path().join("signing.pub"), dir.path().join("root.1.txt"));
        let said = new(&key).expect("minted");
        std::fs::write(&signing, format!("{}\n", repo::public_line(&toyos_update::sig::public_of(&SIGNING)))).unwrap();
        Ceremony { _dir: dir, key, signing, out, said }
    }

    /// The root key is minted `0600`.
    #[test]
    fn the_root_key_is_minted_0600() {
        let c = ceremony();
        assert_eq!(std::fs::metadata(&c.key).unwrap().permissions().mode() & 0o777, 0o600);
    }

    /// A second `new` over the key is refused, and the key is left.
    #[test]
    fn new_over_a_key_is_refused_and_leaves_it() {
        let c = ceremony();
        let before = std::fs::read(&c.key).unwrap();
        assert!(new(&c.key).unwrap_err().ends_with("already exists, and it is not replaced"));
        assert_eq!(std::fs::read(&c.key).unwrap(), before);
    }

    /// `sign` writes the ceremony's root 1 for the minted key and the signing
    /// line, expiring a year from the clock it is given.
    #[test]
    fn sign_writes_the_ceremonys_root_one_expiring_a_year_on() {
        let c = ceremony();
        sign(&c.key, &c.signing, &c.out, NOW).expect("signed");
        let seed = toyos_update::sig::seed_of_text(&std::fs::read_to_string(&c.key).unwrap()).unwrap();
        let expected = repo::render::first_root(&seed, &toyos_update::sig::public_of(&SIGNING), NOW + LIFE);
        assert_eq!(std::fs::read(&c.out).unwrap(), expected);
    }

    /// The root key fingerprint `sign` prints is the one `new` printed.
    #[test]
    fn sign_prints_the_fingerprint_new_printed() {
        let c = ceremony();
        let printed = sign(&c.key, &c.signing, &c.out, NOW).expect("signed");
        let minted = c.said.lines().next().unwrap().strip_prefix("root key ").unwrap();
        assert!(printed.contains(&format!("\nroot key {minted}\n")), "{printed}");
    }

    /// `sign` over a root 1 already written is refused.
    #[test]
    fn sign_over_an_existing_root_is_refused() {
        let c = ceremony();
        std::fs::write(&c.out, b"held").unwrap();
        assert!(sign(&c.key, &c.signing, &c.out, NOW).unwrap_err().ends_with("already exists, and it is not replaced"));
    }

    /// A signing line naming the root key itself is refused, and no root is
    /// written.
    #[test]
    fn a_signing_line_naming_the_root_key_is_refused() {
        let c = ceremony();
        std::fs::write(&c.signing, c.said.lines().nth(1).unwrap()).unwrap();
        assert!(sign(&c.key, &c.signing, &c.out, NOW).unwrap_err().ends_with("names the root key, which holds the root role alone"));
        assert!(!c.out.exists(), "a refused sign wrote no root");
    }

    /// A signing line that is not one Ed25519 public line is refused, and no
    /// root is written.
    #[test]
    fn a_signing_line_that_is_not_one_public_line_is_refused() {
        let c = ceremony();
        std::fs::write(&c.signing, "ssh-ed25519 AAAA comment\n").unwrap();
        assert!(sign(&c.key, &c.signing, &c.out, NOW).unwrap_err().ends_with("is not one `ssh-ed25519 <base64>` line"));
        assert!(!c.out.exists(), "a refused sign wrote no root");
    }
}
