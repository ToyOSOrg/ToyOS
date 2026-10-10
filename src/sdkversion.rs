//! The crates ToyOS publishes, and the version each goes up under.
//!
//! crates.io's is assigned by [`plan`] when `main` publishes, and recorded
//! nowhere but there: `0.N.0+<key>`, the key hashing the crate's git tree and
//! the version assigned to each published crate it depends on. A crate whose
//! key is not its newest published version's takes the next minor, so a change
//! never goes up under a taken version, and a crate keeps its newest only when
//! the manifest going up is the one already up.
//!
//! [`PUBLISHED`]'s order is a dependency order: a crate cannot go up before
//! the index holds every version it names.

use std::path::Path;

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
    Crate { name: "toyos-osrelease", dir: "toyos-osrelease" },
    Crate { name: "toyos-font", dir: "userland/toyos-font" },
    Crate { name: "toyos", dir: "toyos" },
    Crate { name: "toyos-window", dir: "userland/toyos-window" },
];

/// One crate at this tree: the version crates.io has or is owed, whether it is
/// owed, and the manifest it goes up with.
pub struct Release {
    pub krate: &'static Crate,
    pub key: String,
    pub version: String,
    pub publish: bool,
    pub manifest: String,
}

/// Each of [`PUBLISHED`] at `root`'s `HEAD`, against the crates.io index.
pub fn plan(root: &Path) -> Result<Vec<Release>, String> {
    plan_of(PUBLISHED, |krate| source(root, krate), index)
}

/// A crate's `HEAD` tree and its manifest.
fn source(root: &Path, krate: &Crate) -> Result<(String, String), String> {
    let tree = crate::sysroot::git_out(root, &["rev-parse", &format!("HEAD:{}", krate.dir)]).trim().to_string();
    Ok((tree, manifest(root, krate)?))
}

/// Each of `crates` in order: its manifest rewritten with its version and each
/// published dependency's, and the version for its key in its index file.
fn plan_of(
    crates: &'static [Crate],
    source: impl Fn(&Crate) -> Result<(String, String), String>,
    index: impl Fn(&str) -> Result<String, String>,
) -> Result<Vec<Release>, String> {
    let mut plan: Vec<Release> = Vec::new();
    for krate in crates {
        let (mut hashed, text) = source(krate)?;
        let mut manifest: toml::Table =
            text.parse().map_err(|e| format!("{}'s manifest: {e}", krate.name))?;
        let deps = manifest.get_mut("dependencies").and_then(|d| d.as_table_mut());
        for (name, spec) in deps.into_iter().flatten() {
            if !crates.iter().any(|k| k.name == name.as_str()) {
                continue;
            }
            let Some(dep) = plan.iter().find(|r| r.krate.name == name.as_str()) else {
                return Err(format!("{} names {name}, which is not published before it", krate.name));
            };
            hashed.push_str(&dep.version);
            let spec = spec.as_table_mut().ok_or_else(|| format!("{name} is not a path dependency"))?;
            // A requirement's build metadata is ignored, and cargo warns of it.
            spec.insert("version".into(), dep.version.split('+').next().unwrap_or(&dep.version).into());
        }
        let key = crate::release::sha256_hex(hashed.as_bytes())[..16].to_string();
        let (version, publish) = assign(&index(krate.name)?, &key)?;
        let package = manifest.get_mut("package").and_then(|p| p.as_table_mut()).ok_or("no [package]")?;
        package.insert("version".into(), version.clone().into());
        let manifest = toml::to_string(&manifest).map_err(|e| e.to_string())?;
        plan.push(Release { krate, key, version, publish, manifest });
    }
    Ok(plan)
}

/// The version for `key` given the crate's index file: the newest published
/// one if it carries `key` and is not yanked, else the minor after it.
pub(crate) fn assign(index: &str, key: &str) -> Result<(String, bool), String> {
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
    let mut answer = crate::release::agent().get(&url).call().map_err(|e| format!("{url}: {e}"))?;
    let text = answer.body_mut().read_to_string().map_err(|e| format!("{url}: {e}"))?;
    match answer.status().as_u16() {
        404 => Ok(String::new()),
        200 => Ok(text),
        status => Err(format!("the crates.io index answered {status} for {name}: {text}")),
    }
}

/// Every crate's manifest under `root` rewritten as it goes up: its version,
/// and each published dependency's beside its `path`.
pub fn write_published_manifests(root: &Path, plan: &[Release]) -> Result<(), String> {
    for release in plan {
        let path = root.join(release.krate.dir).join("Cargo.toml");
        std::fs::write(&path, &release.manifest).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(())
}

fn manifest(root: &Path, krate: &Crate) -> Result<String, String> {
    let path = root.join(krate.dir).join("Cargo.toml");
    std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gitfixture::{commit, repo};
    use std::collections::BTreeMap;
    use std::process::Command;
    use toyos_tmpdir::TempDir;

    const TWO: &[Crate] =
        &[Crate { name: "toyos-abi", dir: "toyos-abi" }, Crate { name: "toyos", dir: "toyos" }];
    const ABI: &str = "[package]\nname = \"toyos-abi\"\n";
    const TOYOS: &str =
        "[package]\nname = \"toyos\"\n\n[dependencies]\ntoyos-abi = { path = \"../toyos-abi\" }\n";

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
        let (_dir, _origin, wt) = repo("sdk-keys");
        commit(&wt, "toyos-abi/Cargo.toml", ABI, "abi");
        commit(&wt, "toyos/Cargo.toml", TOYOS, "sdk");
        let now = || -> Vec<String> {
            let plan = plan_of(TWO, |k| source(&wt, k), |_| Ok(String::new())).unwrap();
            plan.into_iter().map(|r| r.key).collect()
        };

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

    /// A dependent goes up again when its newest names another version of a
    /// dependency than the one going up: abi went up as B, the run died before
    /// `toyos`, and the next landing put abi back to A.
    #[test]
    fn a_dependent_whose_newest_names_a_stale_dependency_goes_up_again() {
        let mut index = BTreeMap::from([("toyos-abi", String::new()), ("toyos", String::new())]);
        let plan = |abi: &str, index: &BTreeMap<&str, String>| {
            let source = |k: &Crate| {
                let (tree, text) = if k.name == "toyos" { ("T", TOYOS) } else { (abi, ABI) };
                Ok((tree.to_string(), text.to_string()))
            };
            plan_of(TWO, source, |name| Ok(index[name].clone())).unwrap()
        };
        let up = |index: &mut BTreeMap<&str, String>, r: &Release| {
            index.get_mut(r.krate.name).unwrap().push_str(&index_of(&[(&r.version, false)]))
        };
        let first = plan("A", &index);
        up(&mut index, &first[0]);
        up(&mut index, &first[1]);
        let second = plan("B", &index);
        up(&mut index, &second[0]);

        let again = plan("A", &index);
        let (abi, toyos) = (&again[0], &again[1]);
        assert!(abi.publish && toyos.publish, "toyos {} names abi {}", first[1].version, first[0].version);
        let going: toml::Table = toyos.manifest.parse().unwrap();
        assert_eq!(going["dependencies"]["toyos-abi"]["version"].as_str(), abi.version.split('+').next());
        up(&mut index, &again[0]);
        up(&mut index, &again[1]);
        assert!(plan("A", &index).iter().all(|r| !r.publish));
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
        let at = |name: &str| -> String {
            let krate = PUBLISHED.iter().find(|k| k.name == name).unwrap();
            let table: toml::Table = manifest(root, krate).unwrap().parse().unwrap();
            table["package"]["version"].as_str().unwrap().to_string()
        };
        let source = |k: &Crate| Ok(("fedcba9876543210".to_string(), manifest(root, k)?));
        let plan = plan_of(PUBLISHED, source, |name| Ok(index_of(&[(&at(name), false)]))).unwrap();
        for release in &plan {
            let name = release.krate.name;
            assert!(release.publish && release.version != at(name), "{name} kept its version");
            std::fs::create_dir_all(tmp.join(release.krate.dir).join("src")).unwrap();
            std::fs::write(tmp.join(release.krate.dir).join("src/lib.rs"), "").unwrap();
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
             toyos-abi = \">=0.12, <1\"\ntoyos = \">=0.13, <1\"\ntoyos-window = \">=0.15, <1\"\n\
             toyos-osrelease = \">=0.1, <1\"\n\n\
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
        for lockfile in crate::sysroot::tracked_files(root, &["*Cargo.lock"]).unwrap() {
            let lock: toml::Table = std::fs::read_to_string(root.join(&lockfile)).unwrap().parse().unwrap();
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
        plan_of(PUBLISHED, |k| Ok((String::new(), manifest(root, k)?)), |_| Ok(String::new())).unwrap();
        for krate in PUBLISHED {
            let table: toml::Table = manifest(root, krate).unwrap().parse().unwrap();
            let package = table["package"].as_table().unwrap();
            assert_eq!(package["name"].as_str(), Some(krate.name));
            for field in ["version", "description", "repository", "license"] {
                assert!(package.contains_key(field), "{} carries no {field}", krate.dir);
            }
        }
    }
}
