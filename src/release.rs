//! The toolchain a tree builds with: the tag that names it, the builds CI keeps
//! of it, and the release main publishes.
//!
//! **The tag is the content hash of [`trees`]**: the sources a toolchain is
//! built from and the build system's modules that build it ([`BUILDERS`]). A
//! tree that moved none of them names a toolchain somebody already built; a tree
//! that moved any names one nobody has.
//!
//! **CI installs a build only where it came from vouches for it, and only as the
//! bytes it was.** `toolchain.yml` bootstraps a tag no build answers for
//! ([`bootstrap`]) and keeps it as its run's artifact, uploaded whole; GitHub
//! records the SHA-256 of what was uploaded, and nothing rewrites an artifact.
//! [`install`] takes the newest build of its tree's tag that main's publisher
//! made and, failing that, the newest a run of a commit its tree vouches for made
//! ([`vouched`]); it refuses every other, and any download whose bytes hash to
//! anything but GitHub's digest.
//!
//! **Only main publishes** ([`release`]): `publish.yml` on main puts main's own
//! build up as the release a consumer outside CI installs, and moves the SDK
//! alias onto it. Run anywhere else, that job is refused before it reads
//! anything.
//!
//! A build is `x86_64-unknown-linux-gnu`'s and is made on a GitHub-hosted
//! `ubuntu-24.04`; any other host is refused rather than building a tarball
//! nobody can install. A dev host never installs one: its build system
//! bootstraps from `rust/` as always.

use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;
use sha2::{Digest, Sha256};
use toyos_tmpdir::TempDir;

use crate::toolchain::HOSTED_ARCH;

/// The build system's modules that build and pack the toolchain: every module
/// `src/toolchain.rs` and this file name through `crate::`, and every module
/// those name, so the tag moves with how the toolchain is built as well as with
/// what it is built from.
pub(crate) const BUILDERS: [&str; 18] = [
    "src/arch.rs",
    "src/buildlock.rs",
    "src/clang.rs",
    "src/compiler.rs",
    "src/flags.rs",
    "src/gitfixture.rs",
    "src/identity.rs",
    "src/keystore.rs",
    "src/libc.rs",
    "src/libcxx.rs",
    "src/llvm.rs",
    "src/n2.rs",
    "src/release.rs",
    "src/sdkversion.rs",
    "src/sync.rs",
    "src/sysroot.rs",
    "src/toolchain.rs",
    "src/worktree.rs",
];

/// What the tag hashes, as `git rev-parse HEAD:<tree>` names them.
fn trees() -> Vec<&'static str> {
    std::iter::once("rust")
        .chain(crate::sysroot::SYSROOT_SOURCES)
        .chain(crate::sysroot::SYSROOT_MANIFESTS)
        .chain(BUILDERS)
        .collect()
}

/// The release's one asset, under the name a consumer's install fetches.
const ASSET: &str = "toyos-toolchain.tar.zst";

/// Where [`bootstrap`] leaves the build `toolchain.yml` uploads.
pub(crate) const KEPT: &str = "target/toolchain";

/// Main's publisher: the one workflow whose builds every tree takes, and the one
/// [`release`] runs under.
const PUBLISHER: &str = ".github/workflows/publish.yml";

/// The triple a build's host half runs on.
const HOST: &str = "x86_64-unknown-linux-gnu";

/// The oldest glibc a consumer needs: `ubuntu-24.04`'s. A build naming a newer
/// one is refused.
const GLIBC_FLOOR: (u32, u32) = (2, 39);

/// `toolchain-linux-x86_64-<16 hex>`: the first 16 hex digits of the SHA-256 of
/// what `git rev-parse` prints for [`trees`], newline-terminated lines and all.
pub fn tag(root: &Path) -> Result<String, String> {
    let out = Command::new("git")
        .arg("rev-parse")
        .args(trees().iter().map(|t| format!("HEAD:{t}")))
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

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn on_runner() -> bool {
    std::env::var("GITHUB_ACTIONS").is_ok_and(|v| v == "true")
}

fn repo() -> String {
    std::env::var("GITHUB_REPOSITORY").unwrap_or_else(|_| "ToyOSOrg/ToyOS".into())
}

/// Whether this job is main's publisher — [`PUBLISHER`] on main, pushed or
/// dispatched — as the runner names its workflow and event; refused by name if
/// it is not.
fn publisher(workflow: Option<&str>, event: Option<&str>, repo: &str) -> Result<(), String> {
    let mains = format!("{repo}/{PUBLISHER}@refs/heads/main");
    match (workflow, event) {
        (Some(workflow), Some("push" | "workflow_dispatch")) if workflow == mains => Ok(()),
        (workflow, event) => Err(format!(
            "only {mains}, pushed or dispatched, publishes a toolchain, and this job is {} on {}",
            workflow.unwrap_or("no workflow"),
            event.unwrap_or("no event")
        )),
    }
}

fn this_job_publishes() -> bool {
    let var = |name| std::env::var(name).ok();
    publisher(var("GITHUB_WORKFLOW_REF").as_deref(), var("GITHUB_EVENT_NAME").as_deref(), &repo()).is_ok()
}

/// GitHub's answer to `GET https://api.github.com/<path>`, or `None` when there
/// is no such thing.
fn api(path: &str) -> Result<Option<Value>, String> {
    let token = std::env::var("GH_TOKEN").map_err(|_| "GH_TOKEN is unset".to_string())?;
    let url = format!("https://api.github.com/{path}");
    let out = Command::new("curl")
        .args(["-sSL", "--retry", "3", "--retry-all-errors", "-w", "\n%{http_code}"])
        .args(["-H", &format!("Authorization: Bearer {token}")])
        .args(["-H", "Accept: application/vnd.github+json", &url])
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        return Err(format!("curl {url} exited {}: {}", out.status, String::from_utf8_lossy(&out.stderr).trim()));
    }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let (body, status) = text.rsplit_once('\n').unwrap_or(("", text.as_str()));
    match status {
        "200" => serde_json::from_str(body).map(Some).map_err(|e| format!("{url} answered no JSON: {e}")),
        "404" => Ok(None),
        status => Err(format!("{url} answered {status}: {body}")),
    }
}

/// A toolchain build a run kept: its artifact, the digest GitHub recorded of its
/// bytes, and the run that uploaded it.
#[derive(Clone, Debug, PartialEq)]
struct Build {
    artifact: u64,
    /// `sha256:<hex>`.
    digest: String,
    run: u64,
    /// The commit its run was for: the one pushed or queued, or a pull
    /// request's head.
    head: String,
    /// Whether that run was main's publisher's.
    mains: bool,
}

impl Build {
    fn provenance(&self) -> String {
        let whose = if self.mains { "main's publisher" } else { "a commit this tree vouches for" };
        format!("artifact {} of run {}, {whose}, at {}", self.artifact, self.run, self.head)
    }
}

/// The unexpired artifacts in GitHub's `listing`, newest first, each with the
/// branch its run was on. An artifact with no digest to hold its bytes to is
/// none of them.
fn listed(listing: &Value) -> Vec<(Build, String)> {
    let artifacts = listing["artifacts"].as_array().map_or(&[][..], Vec::as_slice);
    artifacts
        .iter()
        .filter(|a| a["expired"] == false)
        .filter_map(|a| {
            let run = &a["workflow_run"];
            let build = Build {
                artifact: a["id"].as_u64()?,
                digest: a["digest"].as_str()?.to_string(),
                run: run["id"].as_u64()?,
                head: run["head_sha"].as_str()?.to_string(),
                mains: false,
            };
            Some((build, run["head_branch"].as_str()?.to_string()))
        })
        .collect()
}

/// Whether GitHub's `run` is main's publisher's: [`PUBLISHER`] on main, pushed
/// or dispatched, in `repo` and from it.
fn is_mains(run: &Value, repo: &str) -> bool {
    run["path"] == PUBLISHER
        && run["head_branch"] == "main"
        && matches!(run["event"].as_str(), Some("push" | "workflow_dispatch"))
        && run["repository"]["full_name"] == repo
        && run["head_repository"]["full_name"] == repo
}

/// Which of `builds`, newest first, a tree installs: the newest main's publisher
/// made, else — unless only main's will do — the newest a run of a commit in
/// `vouched` made.
fn choose<'a>(builds: &'a [Build], vouched: &HashSet<String>, only_mains: bool) -> Option<&'a Build> {
    builds.iter().find(|b| b.mains).or_else(|| {
        if only_mains {
            None
        } else {
            builds.iter().find(|b| vouched.contains(&b.head))
        }
    })
}

/// The commits whose runs' builds this tree takes beside main's publisher's:
/// each commit on its first-parent chain, and the head each merge on that chain
/// took in — main's commits, each head main merged after its review, and a pull
/// request's own head — but no commit a branch passed on its way to the head
/// that was merged.
fn vouched(root: &Path) -> Result<HashSet<String>, String> {
    Ok(merged_heads(&crate::sync::git(root, &["rev-list", "--first-parent", "--parents", "HEAD"])?))
}

/// [`vouched`] read off `git rev-list --first-parent --parents`: each line's
/// commit, and each parent it has but its first.
fn merged_heads(rev_list: &str) -> HashSet<String> {
    rev_list
        .lines()
        .flat_map(|line| {
            let mut words = line.split_whitespace();
            let commit = words.next();
            words.next();
            commit.into_iter().chain(words)
        })
        .map(str::to_string)
        .collect()
}

/// The build of `tag` this tree installs ([`choose`]), if a run kept one.
fn find(root: &Path, tag: &str, only_mains: bool) -> Result<Option<Build>, String> {
    let repo = repo();
    let listing = api(&format!("repos/{repo}/actions/artifacts?name={tag}.tar.zst&per_page=100"))?
        .ok_or_else(|| format!("{repo} lists no artifacts"))?;
    let mut builds = Vec::new();
    for (mut build, branch) in listed(&listing) {
        if branch == "main" {
            let run = api(&format!("repos/{repo}/actions/runs/{}", build.run))?;
            build.mains = run.is_some_and(|run| is_mains(&run, &repo));
        }
        builds.push(build);
    }
    let vouched = if only_mains { HashSet::new() } else { vouched(root)? };
    Ok(choose(&builds, &vouched, only_mains).cloned())
}

/// `cargo run -- --ci toolchain`: whether a build answers for this tree's
/// toolchain, told to the job's next steps as `bootstrap` in `$GITHUB_OUTPUT`.
/// Main's publisher takes only its own.
pub fn toolchain(root: &Path) -> Result<String, String> {
    let output = std::env::var("GITHUB_OUTPUT")
        .map_err(|_| "not a runner: a dev host builds its own toolchain with `cargo run`".to_string())?;
    let tag = tag(root)?;
    let found = find(root, &tag, this_job_publishes())?;
    fs::OpenOptions::new()
        .append(true)
        .open(&output)
        .and_then(|mut file| writeln!(file, "bootstrap={}", found.is_none()))
        .map_err(|e| format!("{output}: {e}"))?;
    Ok(match found {
        Some(build) => format!("{tag}: {}", build.provenance()),
        None => format!("{tag}: no build answers for it, so this job bootstraps one"),
    })
}

/// `cargo run -- --ci bootstrap`: this tree's toolchain built and left in
/// [`KEPT`] for its run to keep, unless a build already answers for it.
pub fn bootstrap(root: &Path) -> Result<String, String> {
    if !on_runner() {
        return Err("not a runner: a dev host builds its own toolchain with `cargo run`".into());
    }
    let tag = tag(root)?;
    if let Some(build) = find(root, &tag, this_job_publishes())? {
        return Ok(format!("{tag}: {}, so nothing is built", build.provenance()));
    }
    let kept = root.join(KEPT);
    fs::create_dir_all(&kept).map_err(|e| format!("{}: {e}", kept.display()))?;
    let tarball = kept.join(format!("{tag}.tar.zst"));
    build(root, &tag, &tarball)?;
    Ok(format!("{tag} built into {}", tarball.display()))
}

/// Install the build of this tree's toolchain it takes ([`find`]) as rustup's
/// `toyos`, on a runner, once its bytes are the ones GitHub recorded.
///
/// Off a runner this says so and does nothing: the build system owns the dev
/// host's toolchain.
pub fn install(root: &Path) -> Result<String, String> {
    if !on_runner() {
        return Ok("not a runner: the build system uses this checkout's own toolchain".into());
    }
    let tag = tag(root)?;
    let build = find(root, &tag, false)?.ok_or_else(|| {
        format!(
            "no build of {tag} answers for this tree: main's publisher kept none, and no run of a \
             commit this tree vouches for kept one. `toolchain.yml` bootstraps it ahead of this job"
        )
    })?;
    let staging = TempDir::new("toolchain-install");
    let tarball = staging.join(ASSET);
    fetch(&build, &tarball)?;
    let into = root.join("rust/build");
    fs::create_dir_all(&into).map_err(|e| format!("{}: {e}", into.display()))?;
    unpack(&tarball, &into)?;
    let stage2 = into.join(format!("{HOST}/stage2"));
    run(Command::new("rustup").args(["toolchain", "link", "toyos"]).arg(&stage2))?;
    run(Command::new(stage2.join("bin/rustc")).arg("-vV"))?;
    Ok(format!("installed {tag} as `toyos`: {}", build.provenance()))
}

/// `build`'s bytes at `to`, held to the digest GitHub recorded of them. Three
/// transfers at most: a body cut short is a wrong digest, not a curl failure.
fn fetch(build: &Build, to: &Path) -> Result<(), String> {
    let mut last = String::new();
    for attempt in 1..=3 {
        match download(build.artifact, to).and_then(|()| verify(to, &build.digest)) {
            Ok(()) => return Ok(()),
            Err(why) => last = why,
        }
        println!("toolchain download attempt {attempt}: {last}");
    }
    Err(format!("artifact {} did not arrive as GitHub recorded it, in three attempts: {last}", build.artifact))
}

/// `artifact`'s bytes into `to`. GitHub answers with a redirect to storage that
/// carries its own credential, and curl sends the token to no other host.
fn download(artifact: u64, to: &Path) -> Result<(), String> {
    let token = std::env::var("GH_TOKEN").map_err(|_| "GH_TOKEN is unset".to_string())?;
    let url = format!("https://api.github.com/repos/{}/actions/artifacts/{artifact}/zip", repo());
    let status = Command::new("curl")
        .args(["-sSfL", "--retry", "3", "--retry-all-errors", "-o"])
        .arg(to)
        .args(["-H", &format!("Authorization: Bearer {token}"), &url])
        .status()
        .map_err(|e| format!("curl: {e}"))?;
    status.success().then_some(()).ok_or_else(|| format!("curl {url} exited {status}"))
}

/// Whether `path` hashes to `digest`, GitHub's `sha256:<hex>`.
fn verify(path: &Path, digest: &str) -> Result<(), String> {
    let want = digest.strip_prefix("sha256:").ok_or_else(|| format!("{digest:?} is not a SHA-256 digest"))?;
    let mut file = fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).map_err(|e| format!("{}: {e}", path.display()))?;
    let got: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if got == want {
        Ok(())
    } else {
        Err(format!("{} hashes to {got}, and GitHub recorded {want}", path.display()))
    }
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

/// `cargo run -- --ci release`: main's own build of its toolchain put up as the
/// release a consumer outside CI installs, and the `sdk-<version>` alias moved
/// onto it. Refused before anything is read unless this job is main's
/// publisher.
pub fn release(root: &Path) -> Result<String, String> {
    let var = |name| std::env::var(name).ok();
    publisher(var("GITHUB_WORKFLOW_REF").as_deref(), var("GITHUB_EVENT_NAME").as_deref(), &repo())?;
    let tag = tag(root)?;
    let build = find(root, &tag, true)?
        .ok_or_else(|| format!("main's publisher kept no build of {tag}: this run's `toolchain` job makes it"))?;
    let tmp = TempDir::new("toolchain-release");
    let tarball = tmp.join(ASSET);
    fetch(&build, &tarball)?;
    let manifest = manifest(root, &tag, &build.head)?;
    fs::write(tmp.join("notes.md"), notes(root, &tag, &manifest)?).map_err(|e| e.to_string())?;
    let put = put_up(root, &tag, &build.digest, &tarball, &tmp.join("notes.md"))?;
    Ok(format!("{put}; {}", alias(root, &manifest, &tmp)?))
}

/// `tag`'s release, made to carry exactly the bytes `digest` names: created if
/// there is none, its asset replaced if another writer's is there, and then held
/// to the digest GitHub records of what it carries.
fn put_up(root: &Path, tag: &str, digest: &str, tarball: &Path, notes: &Path) -> Result<String, String> {
    let path = format!("repos/{}/releases/tags/{tag}", repo());
    let carried = |release: Option<Value>| -> Option<String> {
        let assets = release?["assets"].as_array()?.clone();
        assets.iter().find(|a| a["name"] == ASSET)?["digest"].as_str().map(str::to_string)
    };
    let said = match api(&path)? {
        None => {
            run(Command::new("gh")
                .args(["release", "create", tag, "--title", tag, "--notes-file"])
                .arg(notes)
                .arg(tarball)
                .current_dir(root))?;
            "published"
        }
        Some(release) if carried(Some(release.clone())).as_deref() == Some(digest) => {
            return Ok(format!("{tag} already carries main's build"));
        }
        Some(_) => {
            run(Command::new("gh").args(["release", "upload", tag, "--clobber"]).arg(tarball).current_dir(root))?;
            "had another writer's asset, now main's build"
        }
    };
    let now = carried(api(&path)?);
    if now.as_deref() != Some(digest) {
        return Err(format!("{tag} carries {now:?} after the upload, and main's build is {digest}"));
    }
    Ok(format!("{tag} {said}"))
}

/// Bootstrap this tree's toolchain, hold it to the glibc floor, and pack it into
/// `tarball`.
fn build(root: &Path, tag: &str, tarball: &Path) -> Result<(), String> {
    if !(cfg!(target_os = "linux") && crate::arch::Arch::HOST == Some(crate::arch::Arch::X86_64)) {
        return Err(format!(
            "a build is {HOST}'s and this host is not one; a tarball built here would install \
             nowhere"
        ));
    }
    let toyos = std::env::var("GITHUB_SHA").or_else(|_| crate::sync::git(root, &["rev-parse", "HEAD"]))?;
    let manifest = manifest(root, tag, &toyos)?;
    println!("{manifest}");
    run(Command::new("git").args(["submodule", "update", "--init", "rust"]).current_dir(root))?;
    // Bootstrap takes `HEAD^1` as the upstream commit whose artifacts to fetch
    // when it sees GitHub Actions; in this fork that is our own merge, which
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
    let key = crate::keystore::recorded(root, crate::buildlock::Keyed::Sysroot).ok_or("the build recorded no sysroot key")?;
    let sysroot = format!("sysroots/{key}");
    let stage2 = build.join(&sysroot);
    fs::write(build.join("toyos-sysroot-witness"), crate::sysroot::witness(root))
        .map_err(|e| format!("recording the sysroot's witness: {e}"))?;
    let need = shipped_glibc(&stage2)?;
    if need > GLIBC_FLOOR {
        return Err(format!(
            "the host half needs GLIBC_{}.{} and a build states {}.{}: build it on the oldest \
             supported glibc, or move GLIBC_FLOOR deliberately",
            need.0, need.1, GLIBC_FLOOR.0, GLIBC_FLOOR.1
        ));
    }
    fs::write(build.join("TOOLCHAIN"), &manifest).map_err(|e| e.to_string())?;

    // `lib/rustlib/<host>` and the sysroot's `bin/cargo` are links into this
    // runner's own toolchain; `Owner::Installed` recreates both. GNU tar's
    // `--transform` renames the sysroot to the path an installer links.
    let mut tar = Command::new("tar")
        .arg("-C")
        .arg(&build)
        .arg(format!("--exclude={}/stage2/lib/rustlib/{HOST}", HOSTED_ARCH.userland()))
        .arg(format!("--exclude={sysroot}/bin/cargo"))
        .arg(format!("--transform=s,^{sysroot},{HOST}/stage2,"))
        .args(["-c", &sysroot, &format!("{}/stage2", HOSTED_ARCH.userland())])
        .args(["toyos-sysroot-witness", "TOOLCHAIN"])
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("tar: {e}"))?;
    let stream = tar.stdout.take().expect("piped");
    let zstd = Command::new("zstd").args(["-T0", "-3", "-f", "-o"]).arg(tarball).stdin(stream).status();
    let tar = tar.wait().map_err(|e| format!("tar: {e}"))?;
    let zstd = zstd.map_err(|e| format!("zstd: {e}"))?;
    if !(tar.success() && zstd.success()) {
        return Err(format!("packaging: tar exited {tar}, zstd {zstd}"));
    }
    Ok(())
}

/// Every `GLIBC_x.y` the shipped host binaries and libraries name, as the
/// newest: `rustc` and its libraries, and the `rust-lld`, clang and LLVM tools
/// beside them. A byte scan: it can only over-report, so its failure is a
/// refused build.
fn shipped_glibc(stage2: &Path) -> Result<(u32, u32), String> {
    let mut files: Vec<PathBuf> = Vec::new();
    let tools = stage2.join(format!("lib/rustlib/{HOST}/bin"));
    for (dir, lib) in [(stage2.join("bin"), false), (stage2.join("lib"), true), (tools, false)] {
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

/// `TOOLCHAIN`: the pin a consumer writes down, and what it gets, `toyos` being
/// the commit that built it. Inside the tarball, and the alias release's own
/// asset.
fn manifest(root: &Path, tag: &str, toyos: &str) -> Result<String, String> {
    Ok(format!(
        "toolchain {tag}\ntoyos {toyos}\nrust {}\nhost {HOST}\nglibc {}.{}\n",
        crate::sync::git(root, &["rev-parse", "HEAD:rust"])?,
        GLIBC_FLOOR.0,
        GLIBC_FLOOR.1
    ))
}

/// The release notes: how to install it, what glibc it needs.
fn notes(root: &Path, tag: &str, manifest: &str) -> Result<String, String> {
    let url = format!("https://github.com/{}/releases/download/{tag}/{ASSET}", repo());
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
    cargo +toyos build --target x86_64-unknown-toyos

rustc's ToyOS target names `rust-lld` as its linker, and the toolchain carries it where rustc looks for it, so nothing goes on `PATH`. The `cargo` symlink is not shipped because its path would be the publisher's.

## C

`lib/rustlib/{HOST}/bin/clang` is the clang of the LLVM this `rustc` is built with, from ToyOSOrg/llvm-project, which knows `x86_64-unknown-toyos`. Its C sysroot — the ToyOS C library's headers and its `staticlib` — is `lib/rustlib/x86_64-unknown-toyos/c`, so `clang --target=x86_64-unknown-toyos --sysroot=toyos-toolchain/{HOST}/stage2/lib/rustlib/x86_64-unknown-toyos/c hello.c` builds a ToyOS program, linked by the `ld.lld` beside clang.

## glibc

The host binaries name **GLIBC_{major}.{minor}** at most, so any distribution with glibc {major}.{minor} or newer runs them. A build that needed more is refused rather than published.

## What this is

{indented}
The same lines are the file `TOOLCHAIN` inside the tarball.

## raw-window-handle

Until [rust-windowing/raw-window-handle#223](https://github.com/rust-windowing/raw-window-handle/pull/223) is released, a program that opens a window carries this patch:

    [patch.crates-io]
    {rwh}
"
    ))
}

/// `toolchain-linux-x86_64-sdk-<toyos-abi's version, less its build metadata>`:
/// the name a consumer pins, moved onto this tree's toolchain. A second release
/// carrying only the manifest, because GitHub hangs an asset off one release id.
fn alias(root: &Path, manifest: &str, tmp: &Path) -> Result<String, String> {
    let plan = crate::sdkversion::plan(root)?;
    if let Some(owed) = plan.iter().find(|r| r.publish) {
        let name = owed.krate.name;
        return Err(format!("crates.io holds no {name} of this tree, so no sdk alias can name it"));
    }
    let abi = plan.iter().find(|r| r.krate.name == "toyos-abi").ok_or("toyos-abi is not published")?;
    let abi = abi.version.split('+').next().unwrap_or(&abi.version);
    let alias = format!("toolchain-linux-x86_64-sdk-{abi}");
    let notes = tmp.join("notes.md");
    let toolchain = tmp.join("TOOLCHAIN");
    // The tarball's copy names no crates.io version, so it never depends on crates.io.
    let sdk: String = plan.iter().map(|r| format!("{} {}\n", r.krate.name, r.version)).collect();
    fs::write(&toolchain, format!("{manifest}{sdk}")).map_err(|e| e.to_string())?;
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
    use std::collections::BTreeSet;

    /// The packaging is one of the trees its own tag hashes.
    #[test]
    fn the_tag_hashes_this_file() {
        assert!(trees().contains(&file!()));
        for tree in trees() {
            assert!(
                Path::new(env!("CARGO_MANIFEST_DIR")).join(tree).exists(),
                "{tree} is hashed into the tag and is not in the tree"
            );
        }
    }

    /// Every module `text` names as `crate::<module>`, alone or in a
    /// `crate::{…}` group.
    fn crate_modules(text: &str) -> Vec<String> {
        let ident = |name: &str| -> String {
            name.trim().chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect()
        };
        let mut found = Vec::new();
        for (at, _) in text.match_indices("crate::") {
            let rest = &text[at + "crate::".len()..];
            match rest.strip_prefix('{') {
                Some(group) => found.extend(group.split('}').next().unwrap_or("").split(',').map(ident)),
                None => found.push(ident(rest)),
            }
        }
        found.retain(|name| !name.is_empty());
        found
    }

    /// [`BUILDERS`] is every module `src/toolchain.rs` and this file reach through
    /// `crate::`: a module the toolchain's build starts calling is one more the
    /// tag hashes, and this is what says so.
    #[test]
    fn the_tag_hashes_every_module_that_builds_the_toolchain() {
        let here = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut reached = BTreeSet::new();
        let mut todo = vec!["src/toolchain.rs".to_string(), file!().to_string()];
        while let Some(file) = todo.pop() {
            if !reached.insert(file.clone()) {
                continue;
            }
            let text = fs::read_to_string(here.join(&file)).unwrap_or_else(|e| panic!("{file}: {e}"));
            for module in crate_modules(&text) {
                let path = format!("src/{module}.rs");
                if here.join(&path).is_file() {
                    todo.push(path);
                }
            }
        }
        let declared: BTreeSet<String> = BUILDERS.iter().map(|s| s.to_string()).collect();
        assert_eq!(reached, declared);
    }

    #[test]
    fn a_group_import_names_each_of_its_modules() {
        let text = "use crate::{flags, release::tag, sync};\nlet x = crate::arch::Arch::HOST;";
        assert_eq!(crate_modules(text), ["flags", "release", "sync", "arch"]);
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(["-c", "commit.gpgsign=false", "-c", "user.email=t@t", "-c", "user.name=t"])
            .args(["-c", "init.defaultBranch=main"])
            .args(crate::gitfixture::NO_AUTO_MAINTENANCE)
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A commit to the C and C++ toolchain's declarations or headers, to the n2
    /// it is built under, or to any module that builds it moves the tag.
    #[test]
    fn the_tag_moves_with_what_the_toolchain_is_built_from_and_by() {
        let repo = TempDir::new("release-tag");
        let here = Path::new(env!("CARGO_MANIFEST_DIR"));
        let write = |path: &str, text: &str| {
            let path = repo.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        };
        git(&repo, &["init", "-q"]);
        for tree in trees() {
            match tree {
                "rust" => {
                    git(&repo, &["update-index", "--add", "--cacheinfo", "160000,1111111111111111111111111111111111111111,rust"]);
                }
                file if here.join(file).is_file() => write(file, &fs::read_to_string(here.join(file)).unwrap()),
                dir => write(&format!("{dir}/placeholder"), "x"),
            }
        }
        fs::create_dir_all(repo.join("rust")).unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "the tree"]);
        let mut before = tag(&repo).unwrap();

        let clang = fs::read_to_string(here.join("src/clang.rs")).unwrap();
        let tools = r#"const TOOLS: [&str; 3] = ["llvm-ar", "clang", "ld.lld"];"#;
        let objdump = r#"const TOOLS: [&str; 4] = ["llvm-ar", "clang", "ld.lld", "llvm-objdump"];"#;
        let targets = r#"targets = \"AArch64;X86\""#;
        let riscv = r#"targets = \"AArch64;RISCV;X86\""#;
        assert!(clang.contains(tools) && clang.contains(targets), "src/clang.rs no longer declares what this mutates");
        let with_objdump = clang.replace(tools, objdump);
        let cxx = fs::read_to_string(here.join("src/libcxx.rs")).unwrap();
        let (no_fs, fs_on) = (r#"("LIBCXX_ENABLE_FILESYSTEM", "OFF")"#, r#"("LIBCXX_ENABLE_FILESYSTEM", "ON")"#);
        assert!(cxx.contains(no_fs), "src/libcxx.rs no longer declares what this mutates");
        let n2 = fs::read_to_string(here.join("src/n2.rs")).unwrap();
        let pin = crate::n2::N2[crate::n2::N2.len() - 1];
        assert!(n2.contains(pin), "src/n2.rs no longer declares what this mutates");
        let mut moves = |path: &str, text: String| {
            write(path, &text);
            git(&repo, &["add", "-A"]);
            git(&repo, &["commit", "-qm", "a mutation"]);
            let after = tag(&repo).unwrap();
            assert_ne!(after, before, "a commit to {path} kept the tag");
            before = after;
        };
        moves("src/clang.rs", with_objdump.clone());
        moves("src/clang.rs", with_objdump.replace(targets, riscv));
        moves("src/libcxx.rs", cxx.replace(no_fs, fs_on));
        moves("src/n2.rs", n2.replace(pin, &"0".repeat(pin.len())));
        moves("userland/libc/include/placeholder", "y".to_string());
        for builder in BUILDERS {
            let text = fs::read_to_string(repo.join(builder)).unwrap();
            moves(builder, format!("{text}\nconst MOVED: () = ();\n"));
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

    const REPO: &str = "ToyOSOrg/ToyOS";

    /// The negative control on the publisher: a pull request's job, the merge
    /// queue's, the nightly's, and main's publisher dispatched on a branch or
    /// run by any other event are each refused, by name.
    #[test]
    fn only_mains_publisher_publishes() {
        let mains = format!("{REPO}/.github/workflows/publish.yml@refs/heads/main");
        assert!(publisher(Some(&mains), Some("push"), REPO).is_ok());
        assert!(publisher(Some(&mains), Some("workflow_dispatch"), REPO).is_ok());
        let refused = [
            (format!("{REPO}/.github/workflows/ci.yml@refs/pull/671/merge"), "pull_request"),
            (format!("{REPO}/.github/workflows/ci.yml@refs/heads/gh-readonly-queue/main/pr-671-59052827f"), "merge_group"),
            (format!("{REPO}/.github/workflows/nightly.yml@refs/heads/main"), "schedule"),
            (format!("{REPO}/.github/workflows/publish.yml@refs/heads/wt/toyos-guestci"), "workflow_dispatch"),
            (mains.clone(), "pull_request_target"),
            ("Fork/ToyOS/.github/workflows/publish.yml@refs/heads/main".to_string(), "push"),
        ];
        for (workflow, event) in refused {
            let why = publisher(Some(&workflow), Some(event), REPO).expect_err(&workflow);
            assert!(why.contains(&workflow) && why.contains(event), "{why}");
        }
        assert!(publisher(None, None, REPO).is_err());
    }

    fn run_json(path: &str, branch: &str, event: &str, head_repo: &str) -> Value {
        serde_json::json!({
            "path": path, "head_branch": branch, "event": event,
            "repository": { "full_name": REPO }, "head_repository": { "full_name": head_repo },
        })
    }

    #[test]
    fn mains_builds_are_publish_yml_on_main_and_nothing_else() {
        let publish = ".github/workflows/publish.yml";
        assert!(is_mains(&run_json(publish, "main", "push", REPO), REPO));
        assert!(is_mains(&run_json(publish, "main", "workflow_dispatch", REPO), REPO));
        assert!(!is_mains(&run_json(".github/workflows/nightly.yml", "main", "schedule", REPO), REPO));
        assert!(!is_mains(&run_json(".github/workflows/ci.yml", "main", "pull_request", REPO), REPO));
        assert!(!is_mains(&run_json(publish, "wt/toyos-guestci", "workflow_dispatch", REPO), REPO));
        assert!(!is_mains(&run_json(publish, "main", "pull_request", "Fork/ToyOS"), REPO));
    }

    fn artifact(id: u64, digest: Value, run: u64, head: &str, branch: &str, expired: bool) -> Value {
        serde_json::json!({
            "id": id, "digest": digest, "expired": expired,
            "workflow_run": { "id": run, "head_sha": head, "head_branch": branch },
        })
    }

    /// GitHub's list, read: an expired artifact and one with no digest are not
    /// builds.
    #[test]
    fn a_build_is_an_unexpired_artifact_with_a_digest() {
        let listing = serde_json::json!({ "artifacts": [
            artifact(1, "sha256:aa".into(), 10, "h1", "main", false),
            artifact(3, "sha256:cc".into(), 12, "h3", "wt/y", true),
            artifact(4, Value::Null, 13, "h4", "wt/z", false),
            artifact(5, "sha256:ee".into(), 14, "h5", "wt/q", false),
        ]});
        let got: Vec<(u64, String)> = listed(&listing).into_iter().map(|(b, branch)| (b.artifact, branch)).collect();
        assert_eq!(got, [(1, "main".to_string()), (5, "wt/q".to_string())]);
    }

    fn kept(artifact: u64, head: &str, mains: bool) -> Build {
        Build { artifact, digest: format!("sha256:{artifact}"), run: artifact, head: head.into(), mains }
    }

    /// The negative control on what a tree installs: main's publisher's build
    /// over any newer one, a build a vouched commit's run made after it, and
    /// never one from a commit the tree does not vouch for; and main's publisher
    /// takes nothing but its own.
    #[test]
    fn a_tree_installs_mains_build_else_one_its_history_vouches_for() {
        let vouched: HashSet<String> = ["landed".to_string(), "own".to_string()].into();
        let builds = [kept(4, "passed-through", false), kept(3, "own", false), kept(2, "main", true)];
        assert_eq!(choose(&builds, &vouched, false), Some(&builds[2]));
        assert_eq!(choose(&builds, &vouched, true), Some(&builds[2]));
        let unpublished = [kept(4, "passed-through", false), kept(3, "own", false), kept(1, "landed", false)];
        assert_eq!(choose(&unpublished, &vouched, false), Some(&unpublished[1]));
        assert_eq!(choose(&unpublished, &vouched, true), None);
        assert_eq!(choose(&[kept(4, "passed-through", false)], &vouched, false), None);
    }

    /// A tree vouches for its first-parent chain and the head each merge on it
    /// took in, and for no commit a branch passed through before its head.
    #[test]
    fn a_tree_vouches_for_its_first_parent_chain_and_the_heads_it_merged() {
        let repo = TempDir::new("release-vouched");
        let commit = |message: &str| {
            git(&repo, &["commit", "-q", "--allow-empty", "-m", message]);
            git(&repo, &["rev-parse", "HEAD"])
        };
        git(&repo, &["init", "-q"]);
        let m0 = commit("m0");
        git(&repo, &["switch", "-q", "-c", "landed"]);
        let p0 = commit("p0");
        let p1 = commit("p1");
        git(&repo, &["switch", "-q", "main"]);
        let m1 = commit("m1");
        git(&repo, &["merge", "-q", "--no-ff", "-m", "m2", "landed"]);
        let m2 = git(&repo, &["rev-parse", "HEAD"]);
        git(&repo, &["switch", "-q", "-c", "own", &m1]);
        let q0 = commit("q0");
        let q1 = commit("q1");
        git(&repo, &["switch", "-q", "--detach", &m2]);
        git(&repo, &["merge", "-q", "--no-ff", "-m", "the pull request's merge", "own"]);
        let head = git(&repo, &["rev-parse", "HEAD"]);
        let got = vouched(&repo).unwrap();
        let want: HashSet<String> = [head, m2, m1, m0, p1, q1].into();
        assert_eq!(got, want, "p0 {p0} and q0 {q0} are what a branch passed through");
    }

    /// The negative control on the download: bytes that hash to anything but
    /// GitHub's digest are refused, and so is a digest that is not SHA-256's.
    #[test]
    fn a_download_is_held_to_the_digest_github_recorded() {
        let dir = TempDir::new("release-verify");
        let file = dir.join("toolchain.tar.zst");
        fs::write(&file, b"").unwrap();
        let empty = format!("sha256:{}", sha256_hex(b""));
        assert!(verify(&file, &empty).is_ok());
        fs::write(&file, b"another writer's").unwrap();
        assert!(verify(&file, &empty).unwrap_err().contains("hashes to"));
        assert!(verify(&file, &sha256_hex(b"another writer's")).unwrap_err().contains("is not a SHA-256 digest"));
    }
}
