//! The toolchain a runner builds with, by the build system's own keys, and the
//! release main publishes of it.
//!
//! **A toolchain is four stores, each a cache entry of the key the build system
//! files it under** ([`LAYERS`]). `toolchain.yml` restores each by the key
//! [`toolchain`] wrote, builds what none restored ([`bootstrap`]) and saves only
//! what it built. GitHub's ref scoping is the provenance: an entry a run on main
//! saved is restored on every ref, and any other run saves only into its own
//! ref's scope.
//!
//! **A guest job installs the sysroot its restore step put down** ([`install`]),
//! and main's nightly packs that store into the release a consumer outside CI
//! installs, then moves the SDK alias onto it ([`release`]); run anywhere else,
//! that job is refused before it reads anything.
//!
//! A dev host installs none: its build system builds its own from `rust/`.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use sha2::{Digest, Sha256};
use toyos_tmpdir::TempDir;

use crate::buildlock::Keyed;
use crate::keystore::Key;

const ASSET: &str = "toyos-toolchain.tar.zst";

/// Main's publisher: the one workflow [`release`] runs under.
const PUBLISHER: &str = ".github/workflows/nightly.yml";

const HOST: &str = "x86_64-unknown-linux-gnu";

/// The oldest glibc a consumer needs: `ubuntu-24.04`'s. A build naming a newer
/// one is refused.
const GLIBC_FLOOR: (u32, u32) = (2, 39);

/// What every request this file makes of GitHub says it comes from.
const USER_AGENT: &str = "toyos-build (https://github.com/ToyOSOrg/ToyOS)";

/// The stores a toolchain is, in the order a build makes them, each under the
/// name its cache entry and its job's outputs carry.
const LAYERS: [(Keyed, &str); 4] = [
    (Keyed::Llvm, "llvm"),
    (Keyed::Compiler, "compiler"),
    (Keyed::Freestanding, "freestanding"),
    (Keyed::Sysroot, "sysroot"),
];

/// One of [`LAYERS`] of this tree's toolchain: the key the build system files
/// it under, and the paths it is, relative to the checkout.
struct Layer {
    kind: Keyed,
    name: &'static str,
    key: Key,
    paths: Vec<PathBuf>,
}

impl Layer {
    /// Its cache entry's key.
    fn entry(&self) -> String {
        format!("toolchain-{}-{}", self.name, self.key)
    }
}

/// The release of the sysroot `key` names.
fn tag(key: &Key) -> String {
    format!("toolchain-linux-x86_64-{key}")
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

/// Whether this job is main's publisher — [`PUBLISHER`] on main, scheduled or
/// dispatched — as the runner names its workflow and event; refused by name if
/// it is not.
fn publisher(workflow: Option<&str>, event: Option<&str>, repo: &str) -> Result<(), String> {
    let mains = format!("{repo}/{PUBLISHER}@refs/heads/main");
    match (workflow, event) {
        (Some(workflow), Some("schedule" | "workflow_dispatch")) if workflow == mains => Ok(()),
        (workflow, event) => Err(format!(
            "only {mains}, scheduled or dispatched, publishes a toolchain, and this job is {} on {}",
            workflow.unwrap_or("no workflow"),
            event.unwrap_or("no event")
        )),
    }
}

/// This tree's toolchain as [`LAYERS`], keyed from its sources alone, before
/// any store is there.
fn layers(root: &Path) -> Vec<Layer> {
    let rust_dir = root.join("rust");
    let llvm = crate::llvm::key(&rust_dir);
    let compiler = crate::compiler::primary_key(&rust_dir);
    let freestanding = crate::sysroot::freestanding_key(root, &compiler, &rust_dir);
    let sysroot = crate::sysroot::key(root, &freestanding);
    LAYERS
        .iter()
        .map(|&(kind, name)| {
            let (key, paths) = match kind {
                Keyed::Llvm => (llvm.clone(), vec![crate::llvm::store(&rust_dir).join(&llvm)]),
                Keyed::Compiler => (
                    compiler.clone(),
                    vec![crate::toolchain::stage2(&rust_dir), crate::compiler::primary_record(&rust_dir)],
                ),
                Keyed::Freestanding => {
                    (freestanding.clone(), vec![crate::sysroot::freestanding_dir(&rust_dir).join(&freestanding)])
                }
                Keyed::Sysroot => (sysroot.clone(), vec![crate::sysroot::sysroots_dir(&rust_dir).join(&sysroot)]),
            };
            let paths = paths
                .iter()
                .map(|path| path.strip_prefix(root).unwrap_or_else(|_| panic!("{} is outside the checkout", path.display())))
                .map(Path::to_path_buf)
                .collect();
            Layer { kind, name, key, paths }
        })
        .collect()
}

/// Why `layer` is not whole in `root`, as the build that makes it decides, if
/// it is not.
fn defect(root: &Path, layer: &Layer) -> Option<String> {
    let dir = root.join(&layer.paths[0]);
    match layer.kind {
        Keyed::Llvm => crate::llvm::defect(&dir),
        Keyed::Compiler => match fs::read(root.join(&layer.paths[1])) {
            Ok(record) if Key::of(&record) == layer.key => crate::toolchain::toolchain_defect(&dir),
            _ => Some(format!("{} records no compiler {}", layer.paths[1].display(), layer.key)),
        },
        Keyed::Freestanding => crate::sysroot::unpublished(&dir),
        Keyed::Sysroot => crate::sysroot::unfinished(&dir),
    }
}

/// The file a job's next steps read this step's outputs from: a runner's
/// `$GITHUB_OUTPUT`.
fn step_outputs() -> Result<PathBuf, String> {
    std::env::var_os("GITHUB_OUTPUT")
        .map(PathBuf::from)
        .ok_or_else(|| "not a runner: a dev host builds its own toolchain with `cargo run`".to_string())
}

/// Append `text` to the step outputs at `file`.
fn tell(file: &Path, text: &str) -> Result<(), String> {
    fs::OpenOptions::new()
        .append(true)
        .open(file)
        .and_then(|mut opened| opened.write_all(text.as_bytes()))
        .map_err(|e| format!("{}: {e}", file.display()))
}

/// `cargo run -- --ci toolchain`: each of this tree's [`LAYERS`] as the cache
/// entry its job restores and saves, told to the job's next steps
/// ([`outputs`]).
pub fn toolchain(root: &Path) -> Result<String, String> {
    let file = step_outputs()?;
    crate::ensure_shallow_fork(root)?;
    let layers = layers(root);
    tell(&file, &outputs(&layers))?;
    Ok(layers.iter().map(|layer| format!("{} {}", layer.name, layer.key)).collect::<Vec<_>>().join(", "))
}

/// Each layer's entry as `<name>-key` and its paths, one a line, as
/// `<name>-path`.
fn outputs(layers: &[Layer]) -> String {
    let mut text = String::new();
    for layer in layers {
        let paths: Vec<String> = layer.paths.iter().map(|path| path.display().to_string()).collect();
        text += &format!("{0}-key={1}\n{0}-path<<PATHS\n{2}\nPATHS\n", layer.name, layer.entry(), paths.join("\n"));
    }
    text
}

/// `cargo run -- --ci bootstrap`: this tree's toolchain made whole from what its
/// job restored, and each layer told to the job's save steps as built or kept
/// ([`built`]). A sysroot restored is all a guest job reads, so then nothing is
/// built; and a layer restored and not whole is refused, since its key reads
/// less than its build does.
pub fn bootstrap(root: &Path) -> Result<String, String> {
    let file = step_outputs()?;
    let layers = layers(root);
    let restored: Vec<bool> = layers.iter().map(|layer| root.join(&layer.paths[0]).exists()).collect();
    whole_as_restored(&layers, &restored, |layer| defect(root, layer))?;
    if !restored[3] {
        let mut lock = crate::buildlock::shared(root, "the toolchain");
        drop(crate::toolchain::ensure(root, &mut lock, false));
        for layer in &layers {
            if let Some(why) = defect(root, layer) {
                return Err(format!("the build left {} {} not whole: {why}", layer.name, layer.key));
            }
        }
    }
    tell(&file, &built(&layers, &restored))?;
    let said: Vec<String> = layers
        .iter()
        .zip(&restored)
        .map(|(layer, was)| match (*was, restored[3]) {
            (true, _) => format!("{} {} restored", layer.name, layer.key),
            (false, true) => format!("{} {} not needed", layer.name, layer.key),
            (false, false) => format!("{} {} built", layer.name, layer.key),
        })
        .collect();
    Ok(said.join(", "))
}

/// Refused where a layer `restored` says its job restored is not whole, as
/// `defect` finds it: its key reads less than its build does, and a build
/// under that key could never be saved over the entry.
fn whole_as_restored(layers: &[Layer], restored: &[bool], defect: impl Fn(&Layer) -> Option<String>) -> Result<(), String> {
    for (layer, _) in layers.iter().zip(restored).filter(|(_, restored)| **restored) {
        if let Some(why) = defect(layer) {
            return Err(format!("{} {} was restored and is not whole: {why}", layer.name, layer.key));
        }
    }
    Ok(())
}

/// Each layer as `<name>=built` where `restored` says its job restored neither
/// it nor the sysroot, and `<name>=kept` otherwise: what the job saves.
fn built(layers: &[Layer], restored: &[bool]) -> String {
    let sysroot = restored[3];
    layers
        .iter()
        .zip(restored)
        .map(|(layer, restored)| format!("{}={}\n", layer.name, if *restored || sysroot { "kept" } else { "built" }))
        .collect()
}

/// Install the sysroot its job restored ([`lay_out`]) as rustup's `toyos`, on
/// a runner.
///
/// Off a runner this says so and does nothing: the build system owns the dev
/// host's toolchain.
pub fn install(root: &Path) -> Result<String, String> {
    if !on_runner() {
        return Ok("not a runner: the build system uses this checkout's own toolchain".into());
    }
    let key = lay_out(root)?;
    let stage2 = crate::toolchain::stage2(&root.join("rust"));
    run(Command::new("rustup").args(["toolchain", "link", "toyos"]).arg(&stage2))?;
    run(Command::new(stage2.join("bin/rustc")).arg("-vV"))?;
    Ok(format!("installed sysroot {key} as `toyos`"))
}

/// Lay the one sysroot under `rust/build/sysroots`, which its job's restore
/// step put there, out as this checkout's installed toolchain
/// (`toolchain::Owner::Installed`): at `stage2`, with the witness it records and
/// its [`manifest`]. Refused unless that witness is this tree's.
fn lay_out(root: &Path) -> Result<Key, String> {
    let rust_dir = root.join("rust");
    let store = crate::sysroot::sysroots_dir(&rust_dir);
    let restored = fs::read_dir(&store)
        .and_then(|entries| entries.map(|entry| entry.map(|entry| entry.path())).collect::<std::io::Result<Vec<_>>>())
        .map_err(|e| format!("{}: {e}", store.display()))?;
    let [sysroot] = restored.as_slice() else {
        return Err(format!(
            "{} holds {} entries, and a job installs the one sysroot it restored: {restored:?}",
            store.display(),
            restored.len()
        ));
    };
    let key = sysroot
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(Key::parse)
        .ok_or_else(|| format!("{} is named by no key", sysroot.display()))?;
    let witness = crate::sysroot::recorded_witness(sysroot)?;
    if witness != crate::sysroot::witness(root) {
        return Err(format!(
            "sysroot {key} was built from other sources than this tree's, and this tree's key names it: \
             the key reads less than the sysroot is built from (`src/sysroot.rs`)"
        ));
    }
    let stage2 = crate::toolchain::stage2(&rust_dir);
    let host = stage2.parent().unwrap_or_else(|| panic!("{} has no parent", stage2.display()));
    fs::create_dir_all(host).map_err(|e| format!("{}: {e}", host.display()))?;
    fs::rename(sysroot, &stage2).map_err(|e| format!("{} -> {}: {e}", sysroot.display(), stage2.display()))?;
    for (path, text) in
        [(crate::toolchain::witness_path(&rust_dir), witness), (crate::toolchain::manifest_path(&rust_dir), manifest(&tag(&key)))]
    {
        fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(key)
}

fn run(cmd: &mut Command) -> Result<(), String> {
    let status = cmd.status().map_err(|e| format!("{cmd:?}: {e}"))?;
    status.success().then_some(()).ok_or_else(|| format!("{cmd:?} exited {status}"))
}

/// `cargo run -- --ci release`: the sysroot main's nightly restored, put up as
/// the release a consumer outside CI installs, and the `sdk-<version>` alias
/// moved onto it. Refused before anything is read unless this job is main's
/// publisher, and before anything is put up unless it runs at main's tip.
pub fn release(root: &Path) -> Result<String, String> {
    let var = |name| std::env::var(name).ok();
    release_as(root, var("GITHUB_WORKFLOW_REF").as_deref(), var("GITHUB_EVENT_NAME").as_deref())
}

/// [`release`], run as the job the runner names by its workflow and event.
fn release_as(root: &Path, workflow: Option<&str>, event: Option<&str>) -> Result<String, String> {
    publisher(workflow, event, &repo())?;
    if !(cfg!(target_os = "linux") && crate::arch::Arch::HOST == Some(crate::arch::Arch::X86_64)) {
        return Err(format!("a release is {HOST}'s and this host is not one; a tarball packed here would install nowhere"));
    }
    let tip = crate::sync::git(root, &["ls-remote", "origin", "refs/heads/main"])?;
    crate::ci::at_tip(&tip, crate::sync::git(root, &["rev-parse", "HEAD"])?.trim())?;
    let key = lay_out(root)?;
    let rust_dir = root.join("rust");
    let need = shipped_glibc(&crate::toolchain::stage2(&rust_dir))?;
    if need > GLIBC_FLOOR {
        return Err(format!(
            "the host half needs GLIBC_{}.{} and a release states {}.{}: build it on the oldest supported \
             glibc, or move GLIBC_FLOOR deliberately",
            need.0, need.1, GLIBC_FLOOR.0, GLIBC_FLOOR.1
        ));
    }
    let tmp = TempDir::new("toolchain-release");
    let tarball = tmp.join(ASSET);
    pack(&rust_dir.join("build"), &tarball)?;
    let tag = tag(&key);
    let notes = notes(root, &tag, &manifest(&tag))?;
    let put = put_up(root, &tag, &notes, &tarball, &tmp)?;
    Ok(format!("{put}; {}", alias(root, &tag, &notes, &tmp)?))
}

/// The tarball of the toolchain laid out under `build` ([`lay_out`]) at
/// `tarball`: `<HOST>/stage2` but its `bin/cargo`, which names a path only this
/// runner has, then its witness and `TOOLCHAIN`, in sorted order with no owner
/// or time, so one sysroot packs to one digest.
fn pack(build: &Path, tarball: &Path) -> Result<(), String> {
    let stage2 = Path::new(HOST).join("stage2");
    let mut entries = vec![stage2.clone()];
    walk(&build.join(&stage2), &stage2, &mut entries)?;
    entries.retain(|entry| *entry != stage2.join("bin/cargo"));
    entries.extend(["toyos-sysroot-witness", "TOOLCHAIN"].map(PathBuf::from));
    let mut tar = tar::Builder::new(Vec::new());
    tar.follow_symlinks(false);
    tar.mode(tar::HeaderMode::Deterministic);
    for entry in &entries {
        tar.append_path_with_name(build.join(entry), entry).map_err(|e| format!("pack {}: {e}", entry.display()))?;
    }
    let bytes = tar.into_inner().map_err(|e| format!("pack {}: {e}", tarball.display()))?;
    let file = fs::File::create(tarball).map_err(|e| format!("{}: {e}", tarball.display()))?;
    let mut file = std::io::BufWriter::new(file);
    ruzstd::encoding::compress(bytes.as_slice(), &mut file, ruzstd::encoding::CompressionLevel::Fastest);
    file.flush().map_err(|e| format!("{}: {e}", tarball.display()))
}

/// Every path under `dir`, named as it is under `prefix`, each directory's in
/// sorted order.
fn walk(dir: &Path, prefix: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let mut names = fs::read_dir(dir)
        .and_then(|entries| entries.map(|entry| entry.map(|entry| entry.file_name())).collect::<std::io::Result<Vec<_>>>())
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    names.sort();
    for name in names {
        let path = dir.join(&name);
        out.push(prefix.join(&name));
        let meta = fs::symlink_metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if meta.is_dir() {
            walk(&path, &prefix.join(&name), out)?;
        }
    }
    Ok(())
}

/// What a release needs for its asset `name` to be the bytes `digest` names,
/// given GitHub's account of it, `None` where there is no release.
#[derive(Debug, PartialEq)]
enum Put {
    Create,
    Carried,
    Upload,
    /// Delete the asset another writer put there, then upload.
    Replace { asset: u64 },
}

fn put(release: Option<&Value>, name: &str, digest: &str) -> Result<Put, String> {
    let Some(release) = release else { return Ok(Put::Create) };
    let assets = release["assets"].as_array().ok_or("a release with no assets list")?;
    match assets.iter().find(|asset| asset["name"] == name) {
        None => Ok(Put::Upload),
        Some(asset) if asset["digest"].as_str() == Some(digest) => Ok(Put::Carried),
        Some(asset) => Ok(Put::Replace { asset: asset["id"].as_u64().ok_or("an asset with no id")? }),
    }
}

/// `tag`'s release, made to carry `file` as its asset by its name: created with
/// `notes` where there is none, given them where there is, and then held to the
/// digest GitHub records.
fn put_up(root: &Path, tag: &str, notes: &str, file: &Path, tmp: &Path) -> Result<String, String> {
    let name = file.file_name().and_then(|name| name.to_str()).ok_or_else(|| format!("{} has no name", file.display()))?;
    let digest = format!("sha256:{}", file_sha256(file)?);
    let at = api(&format!("repos/{}/releases/tags/{tag}", repo()));
    let found = github("GET", &at, None)?;
    let (release, said) = match put(found.as_ref(), name, &digest)? {
        Put::Carried => return Ok(format!("{tag} already carries this {name}")),
        Put::Create => {
            let commit = crate::sync::git(root, &["rev-parse", "HEAD"])?;
            let body = serde_json::json!({ "tag_name": tag, "name": tag, "body": notes, "target_commitish": commit.trim() });
            (send(tmp, "POST", &api(&format!("repos/{}/releases", repo())), &body)?, "put up")
        }
        Put::Upload => (renote(tmp, found, notes)?, "given its asset"),
        Put::Replace { asset } => {
            github("DELETE", &api(&format!("repos/{}/releases/assets/{asset}", repo())), None)?;
            (renote(tmp, found, notes)?, "had another writer's asset, now this one")
        }
    };
    let upload = release["upload_url"].as_str().ok_or("a release with no upload URL")?;
    let upload = format!("{}?name={name}", upload.split('{').next().unwrap_or(upload));
    github("POST", &upload, Some((file, "application/octet-stream")))?;
    let now = github("GET", &at, None)?;
    if put(now.as_ref(), name, &digest)? != Put::Carried {
        return Err(format!("{tag} does not carry {digest} as its {name} after the upload"));
    }
    Ok(format!("{tag} {said}"))
}

/// The release `found` given `notes`.
fn renote(tmp: &Path, found: Option<Value>, notes: &str) -> Result<Value, String> {
    let id = found.as_ref().and_then(|release| release["id"].as_u64()).ok_or("a release with no id")?;
    send(tmp, "PATCH", &api(&format!("repos/{}/releases/{id}", repo())), &serde_json::json!({ "body": notes }))
}

/// `body` sent to `url` by `method`, as JSON: what GitHub answered.
fn send(tmp: &Path, method: &str, url: &str, body: &Value) -> Result<Value, String> {
    let file = tmp.join("request.json");
    fs::write(&file, body.to_string()).map_err(|e| format!("{}: {e}", file.display()))?;
    github(method, url, Some((&file, "application/json")))?.ok_or_else(|| format!("{method} {url} found nothing"))
}

fn api(path: &str) -> String {
    format!("https://api.github.com/{path}")
}

/// GitHub's answer to `method` on `url`, sent the file `body` names as its
/// content type where there is one: the JSON it answered, null for no content,
/// and `None` for a 404.
fn github(method: &str, url: &str, body: Option<(&Path, &str)>) -> Result<Option<Value>, String> {
    let token = std::env::var("GH_TOKEN").map_err(|_| "GH_TOKEN is unset".to_string())?;
    let mut curl = Command::new("curl");
    curl.args(["-sSL", "--retry", "3", "-X", method, "-w", "\n%{http_code}", "-A", USER_AGENT])
        .args(["-H", &format!("Authorization: Bearer {token}"), "-H", "Accept: application/vnd.github+json"]);
    if let Some((file, content_type)) = body {
        curl.args(["-H", &format!("Content-Type: {content_type}"), "--data-binary"]).arg(format!("@{}", file.display()));
    }
    let out = curl.arg(url).output().map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        return Err(format!("curl {method} {url} exited {}: {}", out.status, String::from_utf8_lossy(&out.stderr).trim()));
    }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let (answer, status) = text.rsplit_once('\n').unwrap_or(("", text.as_str()));
    match status {
        "200" | "201" => serde_json::from_str(answer).map(Some).map_err(|e| format!("{method} {url} answered no JSON: {e}")),
        "204" => Ok(Some(Value::Null)),
        "404" => Ok(None),
        status => Err(format!("{method} {url} answered {status}: {answer}")),
    }
}

fn file_sha256(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Every `GLIBC_x.y` the shipped host binaries and libraries name, as the
/// newest: `rustc` and its libraries, and the `rust-lld`, clang and LLVM tools
/// beside them.
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

/// `TOOLCHAIN`: the pin a consumer writes down, and what it gets. Inside the
/// tarball, and the start of the alias release's own asset; only what the
/// sysroot's key decides, so one sysroot packs to one digest.
fn manifest(tag: &str) -> String {
    format!("toolchain {tag}\nhost {HOST}\nglibc {}.{}\n", GLIBC_FLOOR.0, GLIBC_FLOOR.1)
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
/// the name a consumer pins, moved onto `tag`. A second release carrying only a
/// `TOOLCHAIN` naming `tag`, the commit that put it up and the SDK crates,
/// because GitHub hangs an asset off one release id.
fn alias(root: &Path, tag: &str, notes: &str, tmp: &Path) -> Result<String, String> {
    let plan = crate::sdkversion::plan(root)?;
    if let Some(owed) = plan.iter().find(|r| r.publish) {
        let name = owed.krate.name;
        return Err(format!("crates.io holds no {name} of this tree, so no sdk alias can name it"));
    }
    let abi = plan.iter().find(|r| r.krate.name == "toyos-abi").ok_or("toyos-abi is not published")?;
    let abi = abi.version.split('+').next().unwrap_or(&abi.version);
    let alias = format!("toolchain-linux-x86_64-sdk-{abi}");
    let commit = |rev: &str| crate::sync::git(root, &["rev-parse", rev]).map(|sha| sha.trim().to_string());
    let sdk: String = plan.iter().map(|r| format!("{} {}\n", r.krate.name, r.version)).collect();
    let toolchain = tmp.join("TOOLCHAIN");
    let text = format!("{}toyos {}\nrust {}\n{sdk}", manifest(tag), commit("HEAD")?, commit("HEAD:rust")?);
    fs::write(&toolchain, text).map_err(|e| format!("{}: {e}", toolchain.display()))?;
    put_up(root, &alias, notes, &toolchain, tmp).map(|said| format!("{said}, naming {tag}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_glibc_scan_takes_the_newest_version_and_nothing_else() {
        let bytes = b"\0GLIBC_2.17\0GLIBC_2.39\0GLIBC_2.4\0GLIBC_PRIVATE\0GLIBC_\0GLIBC_3.\0";
        assert_eq!(glibc_named(bytes), (2, 39));
        assert_eq!(glibc_named(b"GLIBC_2.40"), (2, 40));
        assert!(glibc_named(b"GLIBC_2.40") > GLIBC_FLOOR);
        assert_eq!(glibc_named(b"no version here"), (0, 0));
    }

    const REPO: &str = "ToyOSOrg/ToyOS";

    /// The negative control on the publisher: the release job run as a pull
    /// request's job, the merge queue's, a push's, or main's nightly dispatched
    /// on a branch or run by any other event is refused by name before it reads
    /// anything, and so is another repository's nightly.
    #[test]
    fn only_mains_publisher_publishes() {
        let mains = format!("{REPO}/.github/workflows/nightly.yml@refs/heads/main");
        assert!(publisher(Some(&mains), Some("schedule"), REPO).is_ok());
        assert!(publisher(Some(&mains), Some("workflow_dispatch"), REPO).is_ok());
        let fork = "Fork/ToyOS/.github/workflows/nightly.yml@refs/heads/main";
        assert!(publisher(Some(fork), Some("schedule"), REPO).unwrap_err().contains(fork));
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let repo = repo();
        let refused = [
            (format!("{repo}/.github/workflows/ci.yml@refs/pull/671/merge"), "pull_request"),
            (format!("{repo}/.github/workflows/ci.yml@refs/heads/gh-readonly-queue/main/pr-671-59052827f"), "merge_group"),
            (format!("{repo}/.github/workflows/publish.yml@refs/heads/main"), "push"),
            (format!("{repo}/.github/workflows/nightly.yml@refs/heads/wt/toyos-guestci"), "workflow_dispatch"),
            (format!("{repo}/.github/workflows/nightly.yml@refs/tags/main"), "push"),
            (format!("{repo}/.github/workflows/nightly.yml@refs/heads/main"), "workflow_run"),
        ];
        for (workflow, event) in refused {
            let why = release_as(root, Some(&workflow), Some(event)).expect_err(&workflow);
            assert!(why.starts_with("only ") && why.contains(&workflow) && why.contains(event), "{why}");
        }
        assert!(release_as(root, None, None).unwrap_err().starts_with("only "));
    }

    fn release_json(assets: Value) -> Value {
        serde_json::json!({ "id": 7, "upload_url": "https://uploads.github.com/repos/o/r/releases/7/assets{?name,label}", "assets": assets })
    }

    /// The negative control on what a release is made to carry: none is
    /// created, one with no such asset is given it, one carrying these bytes is
    /// left, and one carrying any other bytes, or bytes GitHub records no digest
    /// for, has its asset replaced.
    #[test]
    fn a_release_is_made_to_carry_these_bytes_and_no_other() {
        let digest = "sha256:aa";
        assert_eq!(put(None, ASSET, digest), Ok(Put::Create));
        let other = serde_json::json!({ "id": 3, "name": "notes.txt", "digest": digest });
        assert_eq!(put(Some(&release_json(serde_json::json!([other]))), ASSET, digest), Ok(Put::Upload));
        let ours = serde_json::json!([{ "id": 4, "name": ASSET, "digest": digest }]);
        assert_eq!(put(Some(&release_json(ours)), ASSET, digest), Ok(Put::Carried));
        let theirs = serde_json::json!([{ "id": 5, "name": ASSET, "digest": "sha256:bb" }]);
        assert_eq!(put(Some(&release_json(theirs)), ASSET, digest), Ok(Put::Replace { asset: 5 }));
        let unrecorded = serde_json::json!([{ "id": 6, "name": ASSET, "digest": null }]);
        assert_eq!(put(Some(&release_json(unrecorded)), ASSET, digest), Ok(Put::Replace { asset: 6 }));
        assert!(put(Some(&serde_json::json!({ "message": "Not Found" })), ASSET, digest).is_err());
    }

    /// A toolchain laid out under `build` as [`lay_out`] leaves one, with a
    /// file, an executable, a directory, a link and a `bin/cargo`.
    fn laid_out(build: &Path, stamp: &str) {
        let stage2 = build.join(HOST).join("stage2");
        for (file, text) in [("bin/rustc", "rustc"), ("lib/libstd.rlib", stamp), ("lib/rustlib/empty/.keep", "")] {
            fs::create_dir_all(stage2.join(file).parent().unwrap()).unwrap();
            fs::write(stage2.join(file), text).unwrap();
        }
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(stage2.join("bin/rustc"), fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink("rustc", stage2.join("bin/ld.lld")).unwrap();
        std::os::unix::fs::symlink("/a/runner/s/cargo", stage2.join("bin/cargo")).unwrap();
        fs::write(build.join("toyos-sysroot-witness"), "toyos-abi/src/lib.rs:00").unwrap();
        fs::write(build.join("TOOLCHAIN"), manifest("toolchain-linux-x86_64-0123456789abcdef")).unwrap();
    }

    /// What the tarball at `path` holds, by name: a link by what it names, a
    /// file by its bytes and mode.
    fn unpacked(path: &Path) -> Vec<(String, String)> {
        let bytes = fs::read(path).unwrap();
        let mut tar = Vec::new();
        ruzstd::decoding::StreamingDecoder::new(bytes.as_slice()).unwrap().read_to_end(&mut tar).unwrap();
        let mut archive = tar::Archive::new(tar.as_slice());
        let mut seen = Vec::new();
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let name = entry.path().unwrap().display().to_string();
            let (kind, mode) = (entry.header().entry_type(), entry.header().mode().unwrap());
            let what = match kind {
                tar::EntryType::Symlink => format!("-> {}", entry.link_name().unwrap().unwrap().display()),
                tar::EntryType::Directory => "dir".to_string(),
                _ => {
                    let mut text = String::new();
                    entry.read_to_string(&mut text).unwrap();
                    format!("{mode:o} {text}")
                }
            };
            seen.push((name, what));
        }
        seen
    }

    use std::io::Read;

    /// **One sysroot packs to one digest, and unpacks whole**: packed again
    /// after its files were written again later, it is the same bytes; it holds
    /// every file, mode and link of `stage2` but `bin/cargo`, then the witness
    /// and `TOOLCHAIN`; and other bytes are another digest.
    #[test]
    fn one_sysroot_packs_to_one_digest_and_unpacks_whole() {
        let tmp = TempDir::new("release-pack");
        let (first, again, other) = (tmp.join("first"), tmp.join("again"), tmp.join("other"));
        laid_out(&first.join("build"), "std");
        laid_out(&again.join("build"), "std");
        laid_out(&other.join("build"), "another std");
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
        fs::File::options().write(true).open(again.join("build").join(HOST).join("stage2/lib/libstd.rlib")).unwrap().set_modified(later).unwrap();
        for dir in [&first, &again, &other] {
            pack(&dir.join("build"), &dir.join(ASSET)).unwrap();
        }
        let digest = |dir: &Path| file_sha256(&dir.join(ASSET)).unwrap();
        assert_eq!(digest(&first), digest(&again), "one sysroot packed to two digests");
        assert_ne!(digest(&first), digest(&other));
        let stage2 = format!("{HOST}/stage2");
        let want: Vec<(String, String)> = [
            (stage2.clone(), "dir"),
            (format!("{stage2}/bin"), "dir"),
            (format!("{stage2}/bin/ld.lld"), "-> rustc"),
            (format!("{stage2}/bin/rustc"), "755 rustc"),
            (format!("{stage2}/lib"), "dir"),
            (format!("{stage2}/lib/libstd.rlib"), "644 std"),
            (format!("{stage2}/lib/rustlib"), "dir"),
            (format!("{stage2}/lib/rustlib/empty"), "dir"),
            (format!("{stage2}/lib/rustlib/empty/.keep"), "644 "),
            ("toyos-sysroot-witness".to_string(), "644 toyos-abi/src/lib.rs:00"),
            ("TOOLCHAIN".to_string(), "644 toolchain toolchain-linux-x86_64-0123456789abcdef\nhost x86_64-unknown-linux-gnu\nglibc 2.39\n"),
        ]
        .map(|(name, what)| (name, what.to_string()))
        .to_vec();
        assert_eq!(unpacked(&first.join(ASSET)), want);
    }

    /// A checkout whose witness reads `abi`, under `root`.
    fn checkout(root: &Path, abi: &str) {
        for tree in crate::sysroot::SYSROOT_SOURCES {
            fs::create_dir_all(root.join(tree)).unwrap();
        }
        fs::write(root.join("toyos-abi/src/lib.rs"), abi).unwrap();
        for manifest in crate::sysroot::SYSROOT_MANIFESTS {
            fs::create_dir_all(root.join(manifest).parent().unwrap()).unwrap();
            fs::write(root.join(manifest), "[package]\n").unwrap();
        }
    }

    /// A sysroot store under `root`'s `rust/build/sysroots`, as a job restores
    /// one, recording `witness`.
    fn restored(root: &Path, key: &str, witness: &str) -> PathBuf {
        let dir = crate::sysroot::sysroots_dir(&root.join("rust")).join(key);
        fs::create_dir_all(dir.join("bin")).unwrap();
        fs::write(dir.join("bin/rustc"), "rustc").unwrap();
        fs::write(dir.join("SOURCES"), format!("{key}\nfork /a/runner/s/rust\n{witness}\n")).unwrap();
        dir
    }

    /// **A job installs the one sysroot it restored, and only one built from
    /// its own tree's sources**: none, two, or one named by no key is refused;
    /// one whose recorded witness is not this tree's is refused; the one that
    /// is lands at `stage2` with that witness and its `TOOLCHAIN`.
    #[test]
    fn a_job_lays_out_the_one_sysroot_it_restored() {
        let tmp = TempDir::new("release-lay-out");
        let root = tmp.join("checkout");
        checkout(&root, "pub struct A;\n");
        fs::create_dir_all(crate::sysroot::sysroots_dir(&root.join("rust"))).unwrap();
        assert!(lay_out(&root).unwrap_err().contains("holds 0 entries"));
        let witness = crate::sysroot::witness(&root);
        let one = restored(&root, "0123456789abcdef", &witness);
        let two = restored(&root, "fedcba9876543210", &witness);
        assert!(lay_out(&root).unwrap_err().contains("holds 2 entries"));
        fs::remove_dir_all(&two).unwrap();
        fs::write(root.join("toyos-abi/src/lib.rs"), "pub struct A(u64);\n").unwrap();
        assert!(lay_out(&root).unwrap_err().contains("built from other sources"), "a sysroot of other sources was laid out");
        fs::write(root.join("toyos-abi/src/lib.rs"), "pub struct A;\n").unwrap();
        assert_eq!(lay_out(&root), Ok(Key::parse("0123456789abcdef").unwrap()));
        let rust_dir = root.join("rust");
        assert!(!one.exists() && crate::toolchain::stage2(&rust_dir).join("bin/rustc").is_file());
        assert_eq!(fs::read_to_string(crate::toolchain::witness_path(&rust_dir)).unwrap(), witness);
        let toolchain = fs::read_to_string(crate::toolchain::manifest_path(&rust_dir)).unwrap();
        assert_eq!(toolchain, manifest("toolchain-linux-x86_64-0123456789abcdef"));
        fs::create_dir_all(rust_dir.join("build/sysroots/not-a-key")).unwrap();
        assert!(lay_out(&root).unwrap_err().contains("named by no key"));
    }

    fn layer(name: &'static str, key: &str, paths: &[&str]) -> Layer {
        let kind = LAYERS.iter().find(|(_, n)| *n == name).unwrap().0;
        Layer { kind, name, key: Key::parse(key).unwrap(), paths: paths.iter().map(PathBuf::from).collect() }
    }

    /// The job's outputs, as GitHub's multiline syntax reads them: each layer's
    /// cache entry, and each path of it on a line of its own.
    #[test]
    fn a_job_is_told_each_layer_s_entry_and_paths() {
        let layers = [
            layer("llvm", "1111111111111111", &["rust/build/llvm/1111111111111111"]),
            layer("compiler", "2222222222222222", &["rust/build/h/stage2", "rust/build/toyos-compiler"]),
        ];
        assert_eq!(
            outputs(&layers),
            "llvm-key=toolchain-llvm-1111111111111111\nllvm-path<<PATHS\nrust/build/llvm/1111111111111111\nPATHS\n\
             compiler-key=toolchain-compiler-2222222222222222\ncompiler-path<<PATHS\nrust/build/h/stage2\n\
             rust/build/toyos-compiler\nPATHS\n"
        );
    }

    /// **A layer restored and not whole is refused, never built again**: its
    /// key reads less than its build does, and a build under that key could
    /// never be saved. A layer not restored is the build's to make.
    #[test]
    fn a_restored_layer_that_is_not_whole_is_refused() {
        let layers: Vec<Layer> =
            LAYERS.iter().enumerate().map(|(at, (_, name))| layer(name, &at.to_string().repeat(16), &["p"])).collect();
        let broken = |layer: &Layer| (layer.name == "compiler").then(|| "stage2 carries no clang".to_string());
        let refused = whole_as_restored(&layers, &[true, true, false, false], broken).unwrap_err();
        assert!(refused.starts_with("compiler 1111111111111111 was restored") && refused.contains("no clang"), "{refused}");
        assert_eq!(whole_as_restored(&layers, &[true, false, false, false], broken), Ok(()));
        assert_eq!(whole_as_restored(&layers, &[true; 4], |_| None), Ok(()));
    }

    /// **Each layer is the store the build system makes, where it makes it**:
    /// the LLVM, the freestanding libraries and the sysroot in their stores by
    /// key, and the primary's compiler as its `stage2` and its record, each
    /// relative to the checkout, keyed before any is built.
    #[test]
    fn the_layers_are_the_stores_the_build_makes() {
        let scratch = TempDir::new("release-layers");
        let (primary, _rust_dir, _) = crate::compiler::tests::estate(&scratch);
        checkout(&primary, "pub struct A;\n");
        let layers = layers(&primary);
        let named: Vec<(&str, Vec<String>)> =
            layers.iter().map(|l| (l.name, l.paths.iter().map(|p| p.display().to_string()).collect())).collect();
        let key = |at: usize| layers[at].key.to_string();
        let host = crate::toolchain::host_triple();
        assert_eq!(
            named,
            [
                ("llvm", vec![format!("rust/build/llvm/{}", key(0))]),
                ("compiler", vec![format!("rust/build/{host}/stage2"), "rust/build/toyos-compiler".to_string()]),
                ("freestanding", vec![format!("rust/build/freestanding/{}", key(2))]),
                ("sysroot", vec![format!("rust/build/sysroots/{}", key(3))]),
            ]
        );
        assert_eq!(layers[1].key, crate::compiler::primary_key(&primary.join("rust")));
    }

    /// **A job saves exactly the layers it built**: none where it restored the
    /// sysroot, which is all a guest job reads, and otherwise each it did not
    /// restore.
    #[test]
    fn a_job_saves_only_what_it_built() {
        let layers: Vec<Layer> = LAYERS
            .iter()
            .enumerate()
            .map(|(at, (_, name))| layer(name, &at.to_string().repeat(16), &["p"]))
            .collect();
        let told = |restored: [bool; 4]| built(&layers, &restored);
        assert_eq!(told([true, true, false, true]), "llvm=kept\ncompiler=kept\nfreestanding=kept\nsysroot=kept\n");
        assert_eq!(told([false, false, false, true]), "llvm=kept\ncompiler=kept\nfreestanding=kept\nsysroot=kept\n");
        assert_eq!(told([true, true, false, false]), "llvm=kept\ncompiler=kept\nfreestanding=built\nsysroot=built\n");
        assert_eq!(told([false; 4]), "llvm=built\ncompiler=built\nfreestanding=built\nsysroot=built\n");
    }
}
