//! The Netstack3 mirror: Google's files from one Fuchsia commit, byte for byte,
//! under [`MIRROR`], and the manifest that pins them, [`MANIFEST`].
//!
//! **The mirror is upstream's tree, not a fork of it.** Nothing under
//! [`MIRROR`] is ever edited here; our packaging — every Cargo manifest and the
//! empty `fidl_fuchsia_net_common` stand-in — lives beside it in
//! `fuchsia/crates/`. The manifest names the Fuchsia commit, the rule that picks
//! the files (each crate directory's `BUILD.gn` and everything under its
//! `src/`, and the root files named whole), and every file's git blob id as
//! Fuchsia's own tree names it at that commit. [`the_mirror_is_upstreams_bytes`]
//! holds the tree to those ids offline: an edited, added or missing file is red
//! by name. Because a blob id is upstream's name for the bytes, anyone can check
//! a pin against `git ls-tree` in a Fuchsia checkout without trusting this
//! repository.
//!
//! [`sync`] is the only writer: `cargo run -- --sync-fuchsia <commit>` fetches
//! that one commit's trees into `target/fuchsia` (depth one, blobs only for the
//! paths the rule picks), replaces [`MIRROR`] with the files the rule selects,
//! and rewrites the manifest's commit and ids. It asks the network, so it runs
//! on demand and in no gate.
//!
//! [`the_mirror_is_upstreams_bytes`]: tests::the_mirror_is_upstreams_bytes

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::pr::git;

/// Where upstream's files live, repository-relative.
pub const MIRROR: &str = "fuchsia/upstream";

/// The pin, repository-relative.
pub const MANIFEST: &str = "fuchsia/upstream.toml";

/// What [`MANIFEST`] holds.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// The repository the commit is fetched from.
    pub remote: String,
    /// The Fuchsia commit every file is taken from.
    pub commit: String,
    /// Files at the root of Fuchsia's tree, taken whole.
    pub files: Vec<String>,
    /// Crate directories: each one's `BUILD.gn` and everything under `src/`.
    pub crates: Vec<String>,
    /// Every mirrored path and its git blob id at `commit`.
    pub blobs: BTreeMap<String, String>,
}

impl Manifest {
    pub fn read(root: &Path) -> Result<Self, String> {
        let path = root.join(MANIFEST);
        let text = std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        toml::from_str(&text).map_err(|e| format!("{MANIFEST}: {e}"))
    }

    fn write(&self, root: &Path) -> Result<(), String> {
        let text = toml::to_string(self).map_err(|e| format!("{MANIFEST}: {e}"))?;
        let head = "# Written by `cargo run -- --sync-fuchsia <commit>` (src/fuchsia.rs); never by hand.\n\n";
        std::fs::write(root.join(MANIFEST), format!("{head}{text}")).map_err(|e| format!("write {MANIFEST}: {e}"))
    }

    /// Whether the rule picks `path`, a path in Fuchsia's tree.
    pub fn selects(&self, path: &str) -> bool {
        self.files.iter().any(|f| f == path)
            || self.crates.iter().any(|dir| {
                path.strip_prefix(dir.as_str())
                    .and_then(|rest| rest.strip_prefix('/'))
                    .is_some_and(|rest| rest == "BUILD.gn" || rest.starts_with("src/"))
            })
    }
}

/// Everything wrong with a mirror holding `found` — each path and the blob id
/// its bytes hash to — against `manifest`, one line each; empty when the mirror
/// is upstream's.
pub fn judge(manifest: &Manifest, found: &BTreeMap<String, String>) -> Vec<String> {
    let mut wrong = Vec::new();
    for (path, id) in found {
        match manifest.blobs.get(path) {
            None => wrong.push(format!("{MIRROR}/{path} is no file of Fuchsia {}'s that {MANIFEST} pins", manifest.commit)),
            Some(pinned) if pinned != id => wrong.push(format!(
                "{MIRROR}/{path} is edited: its bytes are blob {id}, and upstream's at {} are {pinned}",
                manifest.commit
            )),
            Some(_) => {}
        }
    }
    for path in manifest.blobs.keys() {
        if !found.contains_key(path) {
            wrong.push(format!("{MIRROR}/{path} is pinned and missing"));
        }
        if !manifest.selects(path) {
            wrong.push(format!("{MANIFEST} pins {path}, which its own rule does not pick"));
        }
    }
    wrong
}

/// Every file under `dir`, relative to it with `/` separators.
fn files_under(dir: &Path, at: &Path, out: &mut Vec<String>) -> Result<(), String> {
    let entries = std::fs::read_dir(at).map_err(|e| format!("read {}: {e}", at.display()))?;
    for entry in entries {
        let path = entry.map_err(|e| format!("read {}: {e}", at.display()))?.path();
        if path.is_dir() {
            files_under(dir, &path, out)?;
        } else {
            let rel = path.strip_prefix(dir).expect("under the walk's root");
            out.push(rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/"));
        }
    }
    Ok(())
}

/// Every file in the mirror and the blob id its bytes hash to, as git names a
/// blob: no filter, no line-ending conversion, only the bytes on disk.
pub fn found(root: &Path) -> Result<BTreeMap<String, String>, String> {
    let dir = root.join(MIRROR);
    let mut paths = Vec::new();
    files_under(&dir, &dir, &mut paths)?;
    paths.sort();
    let mut args = vec!["hash-object", "--no-filters", "--"];
    args.extend(paths.iter().map(String::as_str));
    let ids = git(&dir, &args)?;
    let ids: Vec<&str> = ids.lines().collect();
    if ids.len() != paths.len() {
        return Err(format!("git hash-object named {} blobs for {} files", ids.len(), paths.len()));
    }
    Ok(paths.into_iter().zip(ids.into_iter().map(String::from)).collect())
}

/// Where [`sync`] keeps its Fuchsia checkout: this worktree's own.
fn checkout(root: &Path) -> PathBuf {
    root.join("target/fuchsia")
}

/// Replace the mirror with Fuchsia `commit`'s files under the manifest's rule,
/// and pin them.
pub fn sync(root: &Path, commit: &str) -> Result<String, String> {
    let old = Manifest::read(root)?;
    let dir = checkout(root);
    if !dir.join(".git").exists() {
        std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        git(&dir, &["init", "--quiet"])?;
        git(&dir, &["remote", "add", "origin", &old.remote])?;
    }
    // One commit's trees, and a blob only when the checkout below needs it.
    git(&dir, &["fetch", "--quiet", "--depth", "1", "--filter=blob:none", "origin", commit])?;
    let mut sparse = vec!["sparse-checkout", "set", "--cone", "--"];
    sparse.extend(old.crates.iter().map(String::as_str));
    git(&dir, &sparse)?;
    git(&dir, &["checkout", "--quiet", "--detach", commit])?;
    let resolved = git(&dir, &["rev-parse", "HEAD"])?;

    let mut listing_args = vec!["ls-tree", "-r", "HEAD", "--"];
    listing_args.extend(old.files.iter().map(String::as_str));
    listing_args.extend(old.crates.iter().map(String::as_str));
    let listing = git(&dir, &listing_args)?;
    let mut blobs = BTreeMap::new();
    for line in listing.lines() {
        let (meta, path) = line.split_once('\t').ok_or_else(|| format!("ls-tree printed {line:?}"))?;
        let [mode, kind, id] = meta.split(' ').collect::<Vec<_>>()[..] else {
            return Err(format!("ls-tree printed {line:?}"));
        };
        if !old.selects(path) {
            continue;
        }
        if (mode, kind) != ("100644", "blob") {
            return Err(format!("{path} is a {kind} of mode {mode} at {resolved}; the mirror holds plain files alone"));
        }
        blobs.insert(path.to_string(), id.to_string());
    }
    for file in &old.files {
        if !blobs.contains_key(file) {
            return Err(format!("Fuchsia {resolved} has no {file}"));
        }
    }
    for krate in &old.crates {
        if !blobs.keys().any(|p| p.starts_with(&format!("{krate}/src/"))) {
            return Err(format!("Fuchsia {resolved} has no {krate}/src"));
        }
    }

    let mirror = root.join(MIRROR);
    if mirror.exists() {
        std::fs::remove_dir_all(&mirror).map_err(|e| format!("remove {}: {e}", mirror.display()))?;
    }
    for path in blobs.keys() {
        let to = mirror.join(path);
        std::fs::create_dir_all(to.parent().expect("a file has a directory"))
            .map_err(|e| format!("create {}: {e}", to.display()))?;
        std::fs::copy(dir.join(path), &to).map_err(|e| format!("copy {path}: {e}"))?;
    }
    let new = Manifest { commit: resolved.clone(), blobs, ..clone_rule(&old) };
    let wrong = judge(&new, &found(root)?);
    if !wrong.is_empty() {
        return Err(format!("the copied mirror is not the fetched tree:\n{}", wrong.join("\n")));
    }
    new.write(root)?;
    let moved = new.blobs.iter().filter(|(p, id)| old.blobs.get(*p) != Some(id)).count();
    let gone = old.blobs.keys().filter(|p| !new.blobs.contains_key(*p)).count();
    Ok(format!(
        "{MIRROR} is Fuchsia {resolved}: {} files, {moved} new or changed and {gone} gone since {}",
        new.blobs.len(),
        old.commit
    ))
}

fn clone_rule(m: &Manifest) -> Manifest {
    Manifest {
        remote: m.remote.clone(),
        commit: m.commit.clone(),
        files: m.files.clone(),
        crates: m.crates.clone(),
        blobs: BTreeMap::new(),
    }
}

/// `cargo run -- --sync-fuchsia <commit>`.
pub fn dispatch(root: &Path, commit: &str) {
    match sync(root, commit) {
        Ok(said) => println!("{said}"),
        Err(why) => {
            eprintln!("Error: {why}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    /// **The mirror is Fuchsia's bytes**: every file under [`MIRROR`] hashes
    /// to the blob id [`MANIFEST`] pins for it, and nothing pinned is missing.
    #[test]
    fn the_mirror_is_upstreams_bytes() {
        let root = repo_root();
        let manifest = Manifest::read(&root).expect("the manifest reads");
        let found = found(&root).expect("the mirror hashes");
        assert!(found.len() > 200, "the walk found {} files, so it is reading no mirror", found.len());
        let wrong = judge(&manifest, &found);
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// Every crate our packaging builds out of the mirror points its library
    /// at a pinned file: a manifest in `fuchsia/crates/` cannot build a file
    /// that is not upstream's.
    #[test]
    fn every_packaged_crate_builds_a_pinned_file() {
        let root = repo_root();
        let manifest = Manifest::read(&root).expect("the manifest reads");
        let crates = root.join("fuchsia/crates");
        let mut reached = 0;
        for entry in std::fs::read_dir(&crates).expect("fuchsia/crates reads") {
            let dir = entry.expect("an entry").path();
            let text = std::fs::read_to_string(dir.join("Cargo.toml")).expect("a crate's manifest");
            let doc: toml::Value = text.parse().expect("a crate's manifest parses");
            let Some(lib) = doc.get("lib").and_then(|l| l.get("path")).and_then(|p| p.as_str()) else {
                continue;
            };
            let Some(path) = lib.strip_prefix("../../upstream/") else {
                assert!(lib.starts_with("src/"), "{} builds {lib}, neither upstream's nor its own", dir.display());
                continue;
            };
            assert!(manifest.blobs.contains_key(path), "{} builds {path}, which is not pinned", dir.display());
            reached += 1;
        }
        assert!(reached >= 20, "only {reached} packaged crates build a mirrored file");
    }

    fn manifest() -> Manifest {
        Manifest {
            remote: "r".into(),
            commit: "c".into(),
            files: vec!["LICENSE".into()],
            crates: vec!["a/b".into()],
            blobs: [("LICENSE", "1"), ("a/b/BUILD.gn", "2"), ("a/b/src/lib.rs", "3")]
                .into_iter()
                .map(|(p, i)| (p.to_string(), i.to_string()))
                .collect(),
        }
    }

    /// The judge's own fixture: an edit, an arrival, a loss and a pin outside
    /// the rule are each red by name, and the pinned tree is not.
    #[test]
    fn an_edited_added_or_missing_file_is_red() {
        let m = manifest();
        assert!(judge(&m, &m.blobs).is_empty());

        let mut edited = m.blobs.clone();
        edited.insert("a/b/src/lib.rs".into(), "9".into());
        let wrong = judge(&m, &edited);
        assert!(wrong.len() == 1 && wrong[0].contains("a/b/src/lib.rs is edited"), "{wrong:?}");

        let mut added = m.blobs.clone();
        added.insert("a/b/src/ours.rs".into(), "4".into());
        assert!(judge(&m, &added)[0].contains("no file of Fuchsia"));

        let mut lost = m.blobs.clone();
        lost.remove("a/b/BUILD.gn");
        assert!(judge(&m, &lost)[0].contains("pinned and missing"));

        let mut outside = manifest();
        outside.blobs.insert("a/b/OWNERS".into(), "5".into());
        assert!(judge(&outside, &outside.blobs)[0].contains("does not pick"));
    }

    #[test]
    fn the_rule_picks_build_files_sources_and_named_root_files() {
        let m = manifest();
        assert!(m.selects("LICENSE"));
        assert!(m.selects("a/b/BUILD.gn"));
        assert!(m.selects("a/b/src/deep/x.rs"));
        assert!(!m.selects("a/b/OWNERS"));
        assert!(!m.selects("a/bc/src/x.rs"));
        assert!(!m.selects("a/b/tests/x.rs"));
        assert!(!m.selects("PATENTS"));
    }
}
