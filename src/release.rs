//! The toolchain release: the tag a tree's toolchain is published under, the
//! tarball it ships as, and how a runner installs one.
//!
//! **The tag is the content hash of everything the tarball's bytes depend on**
//! — [`TREES`]: the rust fork, the three trees compiled into the sysroot, the
//! linker that ships beside them, and this file, which is the packaging. A tree
//! whose toolchain somebody already built finds it published; a tree that moved
//! any of them asks for a tag nobody has, and `cargo run -- --ci toolchain`
//! builds it. Publishing is idempotent because the tag *is* the content.
//!
//! The release is `x86_64-unknown-linux-gnu`'s and is built on a GitHub-hosted
//! `ubuntu-24.04`; any other host is refused rather than publishing a tarball
//! nobody can install. A dev host never installs one: its build system
//! bootstraps from `rust/` as always.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use sha2::{Digest, Sha256};

use crate::toolchain::HOSTED_ARCH;

/// What the tag hashes, as `git rev-parse HEAD:<tree>` names them. The last is
/// this file.
pub const TREES: [&str; 7] = [
    "rust",
    "toyos-abi/src",
    "toyos/src",
    "userland/libc/src",
    "toyos-ld/src",
    "toyos-ld/Cargo.toml",
    "src/release.rs",
];

/// The one asset a release carries.
const ASSET: &str = "toyos-toolchain.tar.zst";

/// The triple the release's host half runs on.
const HOST: &str = "x86_64-unknown-linux-gnu";

/// The oldest glibc a consumer needs: `ubuntu-24.04`'s. A build naming a newer
/// one is refused rather than published.
const GLIBC_FLOOR: (u32, u32) = (2, 39);

/// `toolchain-linux-x86_64-<16 hex>`: the first 16 hex digits of the SHA-256 of
/// what `git rev-parse` prints for [`TREES`], newline-terminated lines and all.
pub fn tag(root: &Path) -> Result<String, String> {
    let out = Command::new("git")
        .arg("rev-parse")
        .args(TREES.iter().map(|t| format!("HEAD:{t}")))
        .current_dir(root)
        .output()
        .map_err(|e| format!("git rev-parse: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git rev-parse of the toolchain's trees: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(format!("toolchain-linux-x86_64-{}", &sha256_hex(&out.stdout)[..16]))
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn on_runner() -> bool {
    std::env::var("GITHUB_ACTIONS").is_ok_and(|v| v == "true")
}

/// The API URL of `tag`'s asset, or `None` while it has none: `gh release
/// create` makes the release before it uploads, so a tag that exists is not yet
/// an installable toolchain. `curl` and not `gh`, because the guest containers
/// carry no `gh`.
fn asset_url(tag: &str) -> Result<Option<String>, String> {
    let repo = std::env::var("GITHUB_REPOSITORY").unwrap_or_else(|_| "ToyOSOrg/ToyOS".into());
    let mut curl = Command::new("curl");
    curl.args(["-sSL", &format!("https://api.github.com/repos/{repo}/releases/tags/{tag}")]);
    if let Ok(token) = std::env::var("GH_TOKEN") {
        curl.args(["-H", &format!("Authorization: Bearer {token}")]);
    }
    let out = curl.output().map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        return Err(format!("curl asked for {tag} and failed: {}", out.status));
    }
    let release: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("GitHub's answer about {tag} is not JSON: {e}"))?;
    Ok(named_asset(&release))
}

/// The `url` of [`ASSET`] in a release's JSON, if it carries one.
fn named_asset(release: &serde_json::Value) -> Option<String> {
    release["assets"]
        .as_array()?
        .iter()
        .find(|a| a["name"] == ASSET)
        .and_then(|a| a["url"].as_str())
        .map(str::to_string)
}

fn sleep(seconds: u64) {
    std::thread::sleep(std::time::Duration::from_secs(seconds));
}

/// Install this tree's published toolchain as rustup's `toyos`, on a runner.
///
/// Off a runner this says so and does nothing: the build system owns the dev
/// host's toolchain.
pub fn install(root: &Path) -> Result<String, String> {
    if !on_runner() {
        return Ok("not a runner: the build system uses this checkout's own toolchain".into());
    }
    if std::env::var("GH_TOKEN").is_err() {
        return Err("GH_TOKEN is unset, and the release download is authenticated".into());
    }
    let tag = tag(root)?;
    let mut url = None;
    for _ in 0..10 {
        url = asset_url(&tag)?;
        if url.is_some() {
            break;
        }
        println!("{tag} carries no {ASSET} yet; asking again in 15 s");
        sleep(15);
    }
    let url = url.ok_or_else(|| {
        format!(
            "{tag} carries no {ASSET}, so there is nothing to install: the nightly's `build` \
             job is what publishes one"
        )
    })?;
    let token = std::env::var("GH_TOKEN").expect("checked above");
    let tarball = std::env::temp_dir().join(ASSET);
    let into = root.join("rust/build");
    fs::create_dir_all(&into).map_err(|e| format!("{}: {e}", into.display()))?;
    // The retry is on the transfer and the unpack together: a truncated body is
    // a `zstd` failure, not a `curl` one.
    let mut last = String::new();
    for attempt in 1..=3 {
        let fetched = Command::new("curl")
            .args(["-sSL", "--retry", "3", "--retry-all-errors", "--retry-delay", "5"])
            .args(["-H", &format!("Authorization: Bearer {token}")])
            .args(["-H", "Accept: application/octet-stream", &url, "-o"])
            .arg(&tarball)
            .status()
            .map_err(|e| format!("curl: {e}"))?;
        match fetched.success().then(|| unpack(&tarball, &into)) {
            Some(Ok(())) => {
                let stage2 = into.join(format!("{HOST}/stage2"));
                run(Command::new("rustup").args(["toolchain", "link", "toyos"]).arg(&stage2))?;
                run(Command::new(stage2.join("bin/rustc")).arg("-vV"))?;
                return Ok(format!("installed {tag} as `toyos`"));
            }
            Some(Err(e)) => last = e,
            None => last = format!("curl exited {fetched}"),
        }
        println!("toolchain download attempt {attempt} failed: {last}");
        sleep(10);
    }
    Err(format!("the toolchain did not download and unpack in three attempts: {last}"))
}

/// `zstd -dc <tarball> | tar -C <into> -x`.
fn unpack(tarball: &Path, into: &Path) -> Result<(), String> {
    let mut zstd = Command::new("zstd")
        .arg("-dc")
        .arg(tarball)
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("zstd: {e}"))?;
    let stream = zstd.stdout.take().expect("piped");
    let tar = Command::new("tar").arg("-C").arg(into).arg("-x").stdin(stream).status();
    let zstd = zstd.wait().map_err(|e| format!("zstd: {e}"))?;
    let tar = tar.map_err(|e| format!("tar: {e}"))?;
    if zstd.success() && tar.success() {
        Ok(())
    } else {
        Err(format!("unpacking: zstd exited {zstd}, tar {tar}"))
    }
}

fn run(cmd: &mut Command) -> Result<(), String> {
    let status = cmd.status().map_err(|e| format!("{cmd:?}: {e}"))?;
    status.success().then_some(()).ok_or_else(|| format!("{cmd:?} exited {status}"))
}

/// Whether `gh` says `tag` carries [`ASSET`].
fn published(root: &Path, tag: &str) -> bool {
    Command::new("gh")
        .args(["release", "view", tag, "--json", "assets", "--jq", ".assets[].name"])
        .current_dir(root)
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).lines().any(|l| l == ASSET))
}

/// `cargo run -- --ci toolchain`: make sure this tree's toolchain is published,
/// building it if nobody has; on `main`, also move the `sdk-<version>` alias a
/// consumer pins onto it.
pub fn ensure_published(root: &Path) -> Result<String, String> {
    let tag = tag(root)?;
    println!("this tree's toolchain: {tag}");
    if !(cfg!(target_os = "linux") && cfg!(target_arch = "x86_64")) {
        return Err(format!(
            "the release is {HOST}'s and this host is not one; a tarball built here would \
             install nowhere"
        ));
    }
    let tmp = std::env::temp_dir();
    let manifest = manifest(root, &tag)?;
    let notes = notes(root, &tag, &manifest)?;
    fs::write(tmp.join("TOOLCHAIN"), &manifest).map_err(|e| e.to_string())?;
    fs::write(tmp.join("notes.md"), &notes).map_err(|e| e.to_string())?;
    println!("{manifest}");

    let mut said = if published(root, &tag) {
        format!("{tag} is already published")
    } else {
        build(root, &tag, &tmp)?;
        format!("{tag} built and published")
    };
    if std::env::var("GITHUB_REF").is_ok_and(|r| r == "refs/heads/main") {
        said.push_str(&format!("; {}", alias(root, &manifest, &tmp)?));
    }
    Ok(said)
}

/// Bootstrap, check the glibc floor, package, publish, and wait for the asset.
fn build(root: &Path, tag: &str, tmp: &Path) -> Result<(), String> {
    run(Command::new("git").args(["submodule", "update", "--init", "rust"]).current_dir(root))?;
    // Bootstrap takes `HEAD^1` as the upstream commit whose LLVM to fetch when
    // it sees GitHub Actions; in this fork that is our own merge, which
    // rust-lang's CI never built.
    run(Command::new("cargo")
        .args(["run", "--", "--build-only"])
        .env_remove("GITHUB_ACTIONS")
        .env_remove("CI")
        .current_dir(root))?;

    // What ships as `{HOST}/stage2` is the sysroot that build compiled against:
    // the compiler with the guest libraries and `libtoyos_c.a` this tree's
    // sources name, recorded beside the witness an installer checks it by.
    let build = root.join("rust/build");
    let key = crate::sysroot::recorded_key(root).ok_or("the build recorded no sysroot key")?;
    let sysroot = format!("sysroots/{key}");
    let stage2 = build.join(&sysroot);
    fs::write(build.join("toyos-sysroot-witness"), crate::sysroot::witness(root))
        .map_err(|e| format!("recording the sysroot's witness: {e}"))?;
    let need = shipped_glibc(&stage2)?;
    if need > GLIBC_FLOOR {
        return Err(format!(
            "the host half needs GLIBC_{}.{} and the release states {}.{}: build it on the \
             oldest supported glibc, or move GLIBC_FLOOR deliberately",
            need.0, need.1, GLIBC_FLOOR.0, GLIBC_FLOOR.1
        ));
    }

    // rustc's ToyOS target finds `toyos-ld` on PATH, so the tarball carries one.
    fs::copy(
        root.join(format!("target/{HOST}/release/toyos-ld")),
        stage2.join("bin/toyos-ld"),
    )
    .map_err(|e| format!("copying toyos-ld into the release: {e}"))?;
    fs::copy(tmp.join("TOOLCHAIN"), build.join("TOOLCHAIN")).map_err(|e| e.to_string())?;

    // `lib/rustlib/<host>` and the sysroot's `bin/cargo` are links into this
    // runner's own toolchain; `Owner::Installed` recreates both. GNU tar's
    // `--transform` renames the sysroot to the path an installer links.
    let tarball = tmp.join(ASSET);
    let mut tar = Command::new("tar")
        .arg("-C")
        .arg(&build)
        .arg(format!("--exclude={}/stage2/lib/rustlib/{HOST}", HOSTED_ARCH.userland()))
        .arg(format!("--exclude={sysroot}/bin/cargo"))
        .arg(format!("--transform=s,^{sysroot},{HOST}/stage2,"))
        .args(["-c", &sysroot, &format!("{}/stage2", HOSTED_ARCH.userland())])
        .args(["toyos-sysroot-witness", "toyos-ld-witness", "TOOLCHAIN"])
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("tar: {e}"))?;
    let stream = tar.stdout.take().expect("piped");
    let zstd = Command::new("zstd").args(["-T0", "-3", "-f", "-o"]).arg(&tarball).stdin(stream).status();
    let tar = tar.wait().map_err(|e| format!("tar: {e}"))?;
    let zstd = zstd.map_err(|e| format!("zstd: {e}"))?;
    if !(tar.success() && zstd.success()) {
        return Err(format!("packaging: tar exited {tar}, zstd {zstd}"));
    }

    let created = Command::new("gh")
        .args(["release", "create", tag, "--title", tag, "--notes-file"])
        .arg(tmp.join("notes.md"))
        .arg(&tarball)
        .current_dir(root)
        .status()
        .map_err(|e| format!("gh: {e}"))?;
    if !created.success() {
        println!("`gh release create {tag}` refused; another run may have published it first");
    }
    for _ in 0..20 {
        if published(root, tag) {
            return Ok(());
        }
        println!("{tag} carries no {ASSET} yet; waiting");
        sleep(15);
    }
    Err(format!("{tag} carries no {ASSET}, so nothing can install this toolchain"))
}

/// Every `GLIBC_x.y` the shipped host binaries and libraries name, as the
/// newest. A byte scan: it can only over-report, so its failure is a refused
/// publish.
fn shipped_glibc(stage2: &Path) -> Result<(u32, u32), String> {
    let mut files: Vec<PathBuf> = Vec::new();
    for (dir, lib) in [(stage2.join("bin"), false), (stage2.join("lib"), true)] {
        let entries = fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for entry in entries.flatten() {
            let path = entry.path();
            let is_file = fs::symlink_metadata(&path).is_ok_and(|m| m.is_file());
            let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
            if is_file && (!lib || name.contains(".so")) {
                files.push(path);
            }
        }
    }
    let mut newest = (0, 0);
    for file in files {
        let bytes = fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        newest = newest.max(glibc_named(&bytes));
    }
    Ok(newest)
}

/// The newest `GLIBC_<major>.<minor>` anywhere in `bytes`, or `(0, 0)`.
fn glibc_named(bytes: &[u8]) -> (u32, u32) {
    const MARK: &[u8] = b"GLIBC_";
    let number = |at: usize| -> (u32, usize) {
        let digits = bytes[at..].iter().take_while(|b| b.is_ascii_digit()).count();
        let text = std::str::from_utf8(&bytes[at..at + digits]).unwrap_or("");
        (text.parse().unwrap_or(0), at + digits)
    };
    let mut newest = (0, 0);
    let mut at = 0;
    while let Some(found) = bytes[at..].windows(MARK.len()).position(|w| w == MARK) {
        let start = at + found + MARK.len();
        at = start;
        let (major, dot) = number(start);
        if dot == start || bytes.get(dot) != Some(&b'.') {
            continue;
        }
        let (minor, end) = number(dot + 1);
        if end > dot + 1 {
            newest = newest.max((major, minor));
        }
    }
    newest
}

/// `TOOLCHAIN`: the pin a consumer writes down, and what it gets. Inside the
/// tarball, and the alias release's own asset.
fn manifest(root: &Path, tag: &str) -> Result<String, String> {
    let head = |rev: &str| crate::pr::git(root, &["rev-parse", rev]);
    let toyos = std::env::var("GITHUB_SHA").or_else(|_| head("HEAD"))?;
    let mut text = format!(
        "toolchain {tag}\ntoyos {toyos}\nrust {}\nhost {HOST}\nglibc {}.{}\n",
        head("HEAD:rust")?,
        GLIBC_FLOOR.0,
        GLIBC_FLOOR.1
    );
    for (name, version, manifest) in crate::sdkversion::versions(root) {
        text.push_str(&format!("{name} {version} {manifest}\n"));
    }
    Ok(text)
}

/// The release notes: how to install it, what glibc it needs, and the SDK
/// crates that go with it.
fn notes(root: &Path, tag: &str, manifest: &str) -> Result<String, String> {
    let repo = std::env::var("GITHUB_REPOSITORY").unwrap_or_else(|_| "ToyOSOrg/ToyOS".into());
    let url = format!("https://github.com/{repo}/releases/download/{tag}/{ASSET}");
    let (major, minor) = GLIBC_FLOOR;
    let userland = fs::read_to_string(root.join("userland/Cargo.toml")).map_err(|e| e.to_string())?;
    let rwh = userland
        .lines()
        .find(|l| l.starts_with("raw-window-handle = "))
        .ok_or("userland/Cargo.toml patches no raw-window-handle")?;
    let indented: String = manifest.lines().map(|l| format!("    {l}\n")).collect();
    Ok(format!(
        "The `{HOST}` toolchain that cross-compiles for `x86_64-unknown-toyos`.

## Install

    mkdir -p toyos-toolchain
    curl -sSL {url} | tar --zstd -x -C toyos-toolchain
    rustup toolchain link toyos toyos-toolchain/{HOST}/stage2
    ln -s \"$(rustup which cargo)\" toyos-toolchain/{HOST}/stage2/bin/cargo
    export PATH=\"$PATH:$PWD/toyos-toolchain/{HOST}/stage2/bin\"
    cargo +toyos build --target x86_64-unknown-toyos

rustc's ToyOS target names its linker `toyos-ld` and finds it on `PATH`; the tarball carries one in that same `bin/`, which goes LAST on `PATH`: first, it shadows rustup's `cargo` proxy with the real binary and `+toyos` fails. The `cargo` symlink is not shipped because its path would be the publisher's. Leaving `PATH` alone: `CARGO_TARGET_X86_64_UNKNOWN_TOYOS_LINKER=<that bin>/toyos-ld`.

## glibc

The host binaries name **GLIBC_{major}.{minor}** at most, so any distribution with glibc {major}.{minor} or newer runs them. A build that needed more is refused rather than published.

## What this is, and the SDK crates that go with it

{indented}
The crate versions are the ones on crates.io this toolchain's std was built from; `sdk-<toyos-abi's version>` is the release tag that names them. The same lines are the file `TOOLCHAIN` inside the tarball.

## raw-window-handle

Until [rust-windowing/raw-window-handle#223](https://github.com/rust-windowing/raw-window-handle/pull/223) is released, a program that opens a window carries this patch:

    [patch.crates-io]
    {rwh}
"
    ))
}

/// `toolchain-linux-x86_64-sdk-<toyos-abi's version>`: the name a consumer
/// pins, moved onto this tree's toolchain. A second release carrying only the
/// manifest, because GitHub hangs an asset off one release id.
fn alias(root: &Path, manifest: &str, tmp: &Path) -> Result<String, String> {
    let abi = manifest
        .lines()
        .find_map(|l| l.strip_prefix("toyos-abi "))
        .and_then(|rest| rest.split_whitespace().next())
        .ok_or("the manifest names no toyos-abi version")?;
    let alias = format!("toolchain-linux-x86_64-sdk-{abi}");
    let notes = tmp.join("notes.md");
    let toolchain = tmp.join("TOOLCHAIN");
    let gh = |args: &[&str], files: &[&Path]| {
        Command::new("gh").args(args).args(files).current_dir(root).status().is_ok_and(|s| s.success())
    };
    let created = gh(&["release", "create", &alias, "--title", &alias, "--notes-file"], &[&notes, &toolchain]);
    let moved = created
        || (gh(&["release", "edit", &alias, "--notes-file"], &[&notes])
            && gh(&["release", "upload", &alias, "--clobber"], &[&toolchain]));
    moved.then(|| format!("{alias} names it")).ok_or_else(|| format!("{alias} could not be moved"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The packaging is one of the trees its own tag hashes.
    #[test]
    fn the_tag_hashes_this_file() {
        assert_eq!(TREES.last(), Some(&file!()));
        for tree in TREES {
            assert!(
                Path::new(env!("CARGO_MANIFEST_DIR")).join(tree).exists(),
                "{tree} is hashed into the tag and is not in the tree"
            );
        }
    }

    /// `sha256sum`'s digest of the same bytes, cut to the same width.
    #[test]
    fn the_hash_is_sha256_of_what_git_printed() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let tag = tag(Path::new(env!("CARGO_MANIFEST_DIR"))).expect("this checkout has a HEAD");
        let hex = tag.strip_prefix("toolchain-linux-x86_64-").expect("the prefix");
        assert!(hex.len() == 16 && hex.bytes().all(|b| b.is_ascii_hexdigit()), "{tag}");
    }

    #[test]
    fn the_glibc_scan_takes_the_newest_version_and_nothing_else() {
        let bytes = b"\0GLIBC_2.17\0GLIBC_2.39\0GLIBC_2.4\0GLIBC_PRIVATE\0GLIBC_\0GLIBC_3.\0";
        assert_eq!(glibc_named(bytes), (2, 39));
        assert_eq!(glibc_named(b"GLIBC_2.40"), (2, 40));
        assert!(glibc_named(b"GLIBC_2.40") > GLIBC_FLOOR);
        assert_eq!(glibc_named(b"no version here"), (0, 0));
    }

    #[test]
    fn an_asset_is_found_by_name_and_a_release_without_it_has_none() {
        let with: serde_json::Value = serde_json::from_str(&format!(
            r#"{{"assets":[{{"name":"other","url":"u1"}},{{"name":"{ASSET}","url":"u2"}}]}}"#
        ))
        .unwrap();
        assert_eq!(named_asset(&with).as_deref(), Some("u2"));
        let without: serde_json::Value = serde_json::from_str(r#"{"assets":[]}"#).unwrap();
        assert_eq!(named_asset(&without), None);
        let missing: serde_json::Value = serde_json::from_str(r#"{"message":"Not Found"}"#).unwrap();
        assert_eq!(named_asset(&missing), None);
    }
}
