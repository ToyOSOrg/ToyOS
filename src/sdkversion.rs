//! The crates ToyOS publishes, and the version each goes up under.
//!
//! Forks name `toyos-abi`, `toyos` and `toyos-window` from crates.io by a range
//! (`">=0.12, <1"`), which the tree's `[patch.crates-io]` answers from the path
//! at the manifest's `version`. That version is never bumped: it is only what
//! the patch answers with. crates.io's is assigned by [`plan`] when `main`
//! publishes, and recorded nowhere but there: `0.N.0+<key>`, the key hashing
//! the crate's git tree and the keys of the published crates it depends on. A
//! crate whose key is not its newest published version's takes the next minor,
//! so a change never goes up under a taken version, and a dependent moves with
//! every dependency that did.
//!
//! [`PUBLISHED`]'s order is a dependency order: a crate cannot go up before
//! the index holds every version it names.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

/// One published crate: the crates.io name, and its repository-relative
/// directory.
pub struct Crate {
    pub name: &'static str,
    pub dir: &'static str,
}

/// The published crates, in the order a publisher must take them.
pub const PUBLISHED: &[Crate] = &[
    Crate { name: "toyos-abi", dir: "toyos-abi" },
    Crate { name: "toyos-keymap", dir: "toyos-keymap" },
    Crate { name: "toyos-font", dir: "userland/toyos-font" },
    Crate { name: "toyos", dir: "toyos" },
    Crate { name: "toyos-window", dir: "userland/toyos-window" },
];

/// One crate at this tree: the version crates.io has or is owed, and whether
/// it is owed.
pub struct Release {
    pub krate: &'static Crate,
    pub version: String,
    pub publish: bool,
}

/// Each of [`PUBLISHED`] at `root`'s `HEAD`, against the crates.io index.
pub fn plan(root: &Path) -> Result<Vec<Release>, String> {
    keys(root, PUBLISHED)?
        .into_iter()
        .map(|(krate, key)| {
            let (version, publish) = assign(&index(krate.name)?, &key)?;
            Ok(Release { krate, version, publish })
        })
        .collect()
}

/// Each crate's key, in `crates`' order: its `HEAD` tree and the keys of the
/// published crates it depends on.
fn keys<'a>(root: &Path, crates: &'a [Crate]) -> Result<Vec<(&'a Crate, String)>, String> {
    let mut keys: Vec<(&Crate, String)> = Vec::new();
    for krate in crates {
        let mut hashed = crate::pr::git(root, &["rev-parse", &format!("HEAD:{}", krate.dir)])?;
        for dep in published_deps(&manifest(root, krate)?)? {
            let Some((_, key)) = keys.iter().find(|(k, _)| k.name == dep) else {
                return Err(format!("{} names {dep}, which is not published before it", krate.name));
            };
            hashed.push_str(key);
        }
        keys.push((krate, crate::release::sha256_hex(hashed.as_bytes())[..16].to_string()));
    }
    Ok(keys)
}

/// The version for `key` given the crate's index file: the newest published
/// one if it carries `key` and is not yanked, else the minor after it.
fn assign(index: &str, key: &str) -> Result<(String, bool), String> {
    let mut newest: Option<((u64, u64, u64), String, bool)> = None;
    for line in index.lines() {
        let entry: serde_json::Value =
            serde_json::from_str(line).map_err(|e| format!("an index line that is not JSON: {e}"))?;
        let vers = entry["vers"].as_str().ok_or("an index line with no `vers`")?;
        let n = numbers(vers).ok_or_else(|| format!("{vers} is not major.minor.patch"))?;
        if newest.as_ref().is_none_or(|(m, _, _)| n > *m) {
            newest = Some((n, vers.to_string(), entry["yanked"] == true));
        }
    }
    Ok(match newest {
        Some((_, vers, false)) if vers.split_once('+').is_some_and(|(_, k)| k == key) => (vers, false),
        Some(((major, minor, _), _, _)) => (format!("{major}.{}.0+{key}", minor + 1), true),
        None => (format!("0.1.0+{key}"), true),
    })
}

fn numbers(vers: &str) -> Option<(u64, u64, u64)> {
    let mut parts = vers.split('+').next()?.split('.').map(|p| p.parse().ok());
    let n = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(n)
}

/// The crates.io sparse index's file for `name`: one JSON object a line, empty
/// for a crate never published.
pub fn index(name: &str) -> Result<String, String> {
    let url = format!("https://index.crates.io/{}/{}/{name}", &name[..2], &name[2..4]);
    let out = Command::new("curl")
        .args(["-sS", "-w", "\n%{http_code}", &url])
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    match text.rsplit_once('\n') {
        Some((_, "404")) => Ok(String::new()),
        Some((body, "200")) => Ok(body.to_string()),
        _ => Err(format!("the crates.io index answered {text:?} for {name}")),
    }
}

/// Every crate's manifest under `root` rewritten as it goes up: its version,
/// and each published dependency's beside its `path`.
pub fn write_published_manifests(root: &Path, plan: &[Release]) -> Result<(), String> {
    let versions: BTreeMap<&str, &str> = plan.iter().map(|r| (r.krate.name, &*r.version)).collect();
    for release in plan {
        let text = published_manifest(&manifest(root, release.krate)?, &release.version, &versions)?;
        let path = root.join(release.krate.dir).join("Cargo.toml");
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(())
}

fn published_manifest(
    text: &str,
    version: &str,
    versions: &BTreeMap<&str, &str>,
) -> Result<String, String> {
    let mut manifest: toml::Table = text.parse().map_err(|e| format!("a manifest: {e}"))?;
    let package = manifest.get_mut("package").and_then(|p| p.as_table_mut()).ok_or("no [package]")?;
    package.insert("version".into(), version.into());
    if let Some(deps) = manifest.get_mut("dependencies").and_then(|d| d.as_table_mut()) {
        for (name, spec) in deps.iter_mut() {
            let Some(at) = versions.get(name.as_str()) else { continue };
            let spec = spec.as_table_mut().ok_or_else(|| format!("{name} is not a path dependency"))?;
            // A requirement's build metadata is ignored, and cargo warns of it.
            spec.insert("version".into(), at.split('+').next().unwrap_or(at).into());
        }
    }
    toml::to_string(&manifest).map_err(|e| e.to_string())
}

fn manifest(root: &Path, krate: &Crate) -> Result<String, String> {
    let path = root.join(krate.dir).join("Cargo.toml");
    std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))
}

/// The published crates `text`'s `[dependencies]` names.
fn published_deps(text: &str) -> Result<Vec<&'static str>, String> {
    let manifest: toml::Table = text.parse().map_err(|e| format!("a manifest: {e}"))?;
    let deps = manifest.get("dependencies").and_then(|d| d.as_table());
    Ok(PUBLISHED
        .iter()
        .map(|k| k.name)
        .filter(|name| deps.is_some_and(|d| d.contains_key(*name)))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pr::tests::{commit, repo};
    use toyos_tmpdir::TempDir;

    fn index_of(lines: &[(&str, bool)]) -> String {
        lines.iter().map(|(v, yanked)| format!("{{\"vers\":\"{v}\",\"yanked\":{yanked}}}\n")).collect()
    }

    /// A changed crate never goes up under a version crates.io has, and an
    /// unchanged one does not go up again.
    #[test]
    fn a_changed_crate_takes_the_minor_after_the_newest_and_an_unchanged_one_keeps_it() {
        let by_hand = index_of(&[("0.16.0", false), ("0.9.0", false)]);
        assert_eq!(assign(&by_hand, "k1").unwrap(), ("0.17.0+k1".into(), true));
        let published = index_of(&[("0.17.0+k1", false), ("0.16.0", false)]);
        assert_eq!(assign(&published, "k1").unwrap(), ("0.17.0+k1".into(), false));
        assert_eq!(assign(&published, "k2").unwrap(), ("0.18.0+k2".into(), true));
        let reverted = index_of(&[("0.17.0+k1", false), ("0.18.0+k2", false)]);
        assert_eq!(assign(&reverted, "k1").unwrap(), ("0.19.0+k1".into(), true));
        let yanked = index_of(&[("0.17.0+k1", true)]);
        assert_eq!(assign(&yanked, "k1").unwrap(), ("0.18.0+k1".into(), true));
        assert_eq!(assign("", "k1").unwrap(), ("0.1.0+k1".into(), true));
        assert!(assign(&index_of(&[("0.1", false)]), "k1").is_err());
    }

    /// The key moves with any file under the crate or under a published crate
    /// it depends on, and with nothing else.
    #[test]
    fn a_key_moves_with_the_crate_and_its_published_dependencies() {
        const TWO: &[Crate] =
            &[Crate { name: "toyos-abi", dir: "toyos-abi" }, Crate { name: "toyos", dir: "toyos" }];
        let (_dir, _origin, wt) = repo("sdk-keys");
        commit(&wt, "toyos-abi/Cargo.toml", "[package]\nname = \"toyos-abi\"\n", "abi");
        let toyos = "[package]\nname = \"toyos\"\n\n[dependencies]\ntoyos-abi = { path = \"../toyos-abi\" }\n";
        commit(&wt, "toyos/Cargo.toml", toyos, "sdk");
        let now = || -> Vec<String> { keys(&wt, TWO).unwrap().into_iter().map(|(_, k)| k).collect() };

        let base = now();
        commit(&wt, "kernel/src/lib.rs", "// work\n", "elsewhere");
        assert_eq!(now(), base);
        commit(&wt, "toyos/src/lib.rs", "pub struct T;\n", "sdk source");
        let sdk = now();
        assert!(sdk[0] == base[0] && sdk[1] != base[1], "{sdk:?} against {base:?}");
        commit(&wt, "toyos-abi/tests/a.rs", "#[test]\nfn a() {}\n", "abi test");
        let abi = now();
        assert!(abi[0] != sdk[0] && abi[1] != sdk[1], "{abi:?} against {sdk:?}");
    }

    /// **cargo is the judge**: the tree's manifests as they go up after every
    /// crate changed, served as crates.io would serve them, resolved offline
    /// with no registry at all by a consumer naming the three as the forks do.
    /// Each resolves to its new version and that one alone, so every
    /// dependent's rewritten requirement names it too.
    #[test]
    fn the_published_manifests_resolve_as_the_forks_name_them() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let tmp = TempDir::new("sdk-resolve");
        let mut plan = Vec::new();
        for krate in PUBLISHED {
            let text = manifest(root, krate).unwrap();
            let table: toml::Table = text.parse().unwrap();
            let at = table["package"]["version"].as_str().unwrap();
            let (version, publish) = assign(&index_of(&[(at, false)]), "fedcba9876543210").unwrap();
            assert!(publish && version != at, "{} changed and kept {at}", krate.name);
            plan.push(Release { krate, version, publish });
            std::fs::create_dir_all(tmp.join(krate.dir).join("src")).unwrap();
            std::fs::write(tmp.join(krate.dir).join("Cargo.toml"), text).unwrap();
            std::fs::write(tmp.join(krate.dir).join("src/lib.rs"), "").unwrap();
        }
        write_published_manifests(&tmp, &plan).unwrap();
        // As crates.io serves it: `cargo publish` drops every `path`.
        for krate in PUBLISHED {
            let path = tmp.join(krate.dir).join("Cargo.toml");
            let mut table: toml::Table = std::fs::read_to_string(&path).unwrap().parse().unwrap();
            let deps = table.get_mut("dependencies").and_then(|d| d.as_table_mut());
            for spec in deps.into_iter().flat_map(|d| d.iter_mut().map(|(_, s)| s)).filter_map(|s| s.as_table_mut()) {
                spec.remove("path");
            }
            std::fs::write(&path, toml::to_string(&table).unwrap()).unwrap();
        }

        let patch: String =
            PUBLISHED.iter().map(|k| format!("{} = {{ path = \"../{}\" }}\n", k.name, k.dir)).collect();
        std::fs::create_dir_all(tmp.join("fork/src")).unwrap();
        std::fs::write(tmp.join("fork/src/lib.rs"), "").unwrap();
        let fork = format!(
            "[package]\nname = \"fork\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[dependencies]\n\
             toyos-abi = \">=0.12, <1\"\ntoyos = \">=0.13, <1\"\ntoyos-window = \">=0.15, <1\"\n\n\
             [patch.crates-io]\n{patch}"
        );
        std::fs::write(tmp.join("fork/Cargo.toml"), fork).unwrap();
        let out = Command::new("cargo")
            .args(["metadata", "--offline", "--format-version", "1"])
            .env("CARGO_HOME", tmp.join("cargo-home"))
            .current_dir(tmp.join("fork"))
            .output()
            .expect("run cargo");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let metadata: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        let packages = metadata["packages"].as_array().unwrap();
        for release in &plan {
            let resolved: Vec<&str> = packages
                .iter()
                .filter(|p| p["name"] == release.krate.name)
                .map(|p| p["version"].as_str().unwrap())
                .collect();
            assert_eq!(resolved, [&*release.version], "{}", release.krate.name);
        }
    }

    /// The tree's own half: every lockfile answers each published crate from
    /// its path, never from a registry beside it.
    #[test]
    fn every_lockfile_resolves_the_published_crates_from_the_tree() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut seen = 0;
        for lockfile in crate::pr::git(root, &["ls-files", "*Cargo.lock"]).unwrap().lines() {
            let lock: toml::Table = std::fs::read_to_string(root.join(lockfile)).unwrap().parse().unwrap();
            for package in lock.get("package").and_then(|p| p.as_array()).into_iter().flatten() {
                let name = package["name"].as_str().unwrap();
                if PUBLISHED.iter().any(|k| k.name == name) {
                    seen += 1;
                    assert!(package.get("source").is_none(), "{lockfile} takes {name} from a registry");
                }
            }
        }
        assert!(seen > 10, "{seen} published crates across the lockfiles is not the tree");
    }

    /// The order is the one a publisher must take, and each row names a
    /// directory whose package is the row's name and carries what crates.io asks.
    #[test]
    fn every_row_is_a_crate_the_tree_holds_in_dependency_order() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        for (n, krate) in PUBLISHED.iter().enumerate() {
            let text = manifest(root, krate).unwrap();
            let table: toml::Table = text.parse().unwrap();
            let package = table["package"].as_table().unwrap();
            assert_eq!(package["name"].as_str(), Some(krate.name));
            for field in ["version", "description", "repository", "license"] {
                assert!(package.contains_key(field), "{} carries no {field}", krate.dir);
            }
            for dep in published_deps(&text).unwrap() {
                let at = PUBLISHED.iter().position(|k| k.name == dep).unwrap();
                assert!(at < n, "{} names {dep}, which is not published before it", krate.name);
            }
        }
    }
}
