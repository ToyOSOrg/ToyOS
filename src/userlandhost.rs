//! Which userland crates `cargo run -- --ci host` tests, and every userland test
//! it would run nowhere.
//!
//! `userland/` is a workspace of its own that cross-compiles by default
//! (`userland/.cargo/config.toml`), so none of its crates can be a member of the
//! host workspace [`crate::hostws`] holds; the gate runs `cargo test --target
//! <host>` in each crate [`survey`] gates instead. There is no list: a crate's
//! first test gates the merge that adds it.
//!
//! **A test is found by reading text**, because cargo's metadata knows targets
//! and not tests, and the one tool that lists tests, the test binary, needs a
//! host build most userland crates cannot make. So the reading is made to fail
//! loudly: a test is gated only in the one shape a default `cargo test` in a
//! crate directly under `userland/` is known to run. That is an attribute naming
//! `test` or `test_case`, in a file under the crate's `src/` or `tests/`, in a
//! crate whose only `cfg` is `cfg(test)`, which ignores no test, and whose
//! manifest switches off no target's tests.
//!
//! Doc-tests are not read: a library crate that leaves `doctest` on is refused,
//! because the `toyos` toolchain builds no rustdoc.
//!
//! A test in a `src/` file that no `mod` reaches is gated and never compiled.
//! That escape is not closed here.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// What [`survey`] found under one `userland/` directory.
#[derive(Debug, PartialEq, Eq)]
pub struct Survey {
    /// The crates `host` runs, by directory name, sorted.
    pub gated: Vec<String>,
    /// Every test the gate would not run, each as `<path under userland>: why`,
    /// sorted. `host` is red while this is not empty.
    pub escapes: Vec<String>,
}

/// Every crate directly under `userland` that holds a test, and every test that
/// no `cargo test` in one of them runs.
pub fn survey(userland: &Path) -> Result<Survey, String> {
    let mut files = Vec::new();
    rs_files(userland, &mut files)?;
    files.sort();
    let mut crates = BTreeSet::new();
    let mut gated = BTreeSet::new();
    let mut escapes = BTreeSet::new();
    // Refusals that bind only once the file's crate turns out to be gated.
    let mut conditional: Vec<(String, PathBuf, &'static str)> = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
        let at = rel(userland, file);
        let scan = scan(&text).map_err(|why| format!("{at}: {why}"))?;
        let Some(owner) = file
            .ancestors()
            .skip(1)
            .take_while(|dir| *dir != userland)
            .find(|dir| dir.join("Cargo.toml").is_file())
        else {
            if scan.test {
                escapes.insert(format!("{at}: a test in no crate"));
            }
            continue;
        };
        let crate_name = rel(userland, owner);
        crates.insert(crate_name.clone());
        for why in &scan.conditions {
            conditional.push((crate_name.clone(), file.clone(), *why));
        }
        if !scan.test {
            continue;
        }
        let inside = rel(owner, file);
        if crate_name.contains('/') {
            escapes.insert(format!(
                "{at}: a test in the nested crate {crate_name}, which the gate does not discover"
            ));
        } else if !(inside.starts_with("src/") || inside.starts_with("tests/")) {
            escapes.insert(format!(
                "{at}: a test outside {crate_name}'s src/ and tests/, which cargo test does not run"
            ));
        } else {
            gated.insert(crate_name);
        }
    }
    for (crate_name, file, why) in conditional {
        if gated.contains(&crate_name) {
            escapes.insert(format!("{}: {why}", rel(userland, &file)));
        }
    }
    for crate_name in &crates {
        let manifest = userland.join(crate_name).join("Cargo.toml");
        let text =
            std::fs::read_to_string(&manifest).map_err(|e| format!("{}: {e}", manifest.display()))?;
        let doc: toml::Value =
            text.parse().map_err(|e| format!("{}: not TOML: {e}", manifest.display()))?;
        let lib = doc.get("lib");
        let has_lib = lib.is_some() || userland.join(crate_name).join("src/lib.rs").is_file();
        if has_lib && lib.and_then(|l| l.get("doctest")).and_then(toml::Value::as_bool) != Some(false) {
            escapes.insert(format!(
                "{crate_name}/Cargo.toml: a library without [lib] doctest = false, whose doc-tests the gate does not read"
            ));
        }
        if gated.contains(crate_name) {
            for key in switched_off(&doc) {
                escapes.insert(format!("{crate_name}/Cargo.toml: {key}"));
            }
        }
    }
    Ok(Survey { gated: gated.into_iter().collect(), escapes: escapes.into_iter().collect() })
}

/// `path` under `base`, with forward slashes.
fn rel(base: &Path, path: &Path) -> String {
    path.strip_prefix(base).unwrap_or(path).to_string_lossy().replace('\\', "/")
}

/// Every `.rs` file under `dir`, which may not exist; `target` and dotted
/// directories are build output and history.
fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    };
    for entry in entries {
        let path = entry.map_err(|e| format!("{}: {e}", dir.display()))?.path();
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        if path.is_dir() {
            if !name.starts_with('.') && name != "target" {
                rs_files(&path, out)?;
            }
        } else if name.ends_with(".rs") {
            out.push(path);
        }
    }
    Ok(())
}

/// The manifest keys that stop a default `cargo test` from running a test the
/// crate holds.
fn switched_off(doc: &toml::Value) -> Vec<String> {
    let mut found = Vec::new();
    if let Some(package) = doc.get("package") {
        for key in ["autolib", "autobins", "autotests"] {
            if package.get(key).and_then(toml::Value::as_bool) == Some(false) {
                found.push(format!("[package] {key} = false leaves a target's tests unbuilt"));
            }
        }
    }
    let tables = doc.get("lib").into_iter().map(|t| ("lib", t)).chain(
        ["bin", "test"].into_iter().flat_map(|kind| {
            doc.get(kind)
                .and_then(toml::Value::as_array)
                .into_iter()
                .flatten()
                .map(move |t| (kind, t))
        }),
    );
    for (kind, table) in tables {
        for key in ["test", "harness"] {
            if table.get(key).and_then(toml::Value::as_bool) == Some(false) {
                found.push(format!("[{kind}] {key} = false leaves its tests unrun"));
            }
        }
        if table.get("required-features").is_some() {
            found.push(format!("[{kind}] required-features leaves it unbuilt by default"));
        }
    }
    found
}

/// What one source file holds, as far as the gate is concerned.
#[derive(Debug, Default, PartialEq, Eq)]
struct Scan {
    /// An attribute names `test` or `test_case` as a word.
    test: bool,
    /// Why a default `cargo test` might skip this crate's tests; binding only
    /// if the crate is gated.
    conditions: Vec<&'static str>,
}

/// Read every attribute in `text` for a test and for what could hide one.
fn scan(text: &str) -> Result<Scan, &'static str> {
    let mut out = Scan::default();
    let mut attribute: Option<(String, i32)> = None;
    for line in text.lines() {
        let mut rest = line.trim_start();
        loop {
            if attribute.is_none() {
                if !(rest.starts_with("#[") || rest.starts_with("#![")) {
                    break;
                }
                attribute = Some((String::new(), 0));
            }
            let (body, depth) = attribute.as_mut().expect("inside an attribute");
            let mut in_string = false;
            let mut escaped = false;
            let mut end = None;
            for (i, c) in rest.char_indices() {
                if in_string {
                    if escaped {
                        escaped = false;
                    } else if c == '\\' {
                        escaped = true;
                    } else if c == '"' {
                        in_string = false;
                        body.push('"');
                    }
                    continue;
                }
                match c {
                    '"' => {
                        in_string = true;
                        body.push('"');
                        continue;
                    }
                    '[' => *depth += 1,
                    ']' => *depth -= 1,
                    _ => {}
                }
                if !c.is_whitespace() {
                    body.push(c);
                }
                if *depth == 0 && c == ']' {
                    end = Some(i + 1);
                    break;
                }
            }
            let Some(end) = end else {
                body.push(' ');
                break;
            };
            let (body, _) = attribute.take().expect("inside an attribute");
            judge_attribute(&body, &mut out);
            rest = rest[end..].trim_start();
        }
    }
    match attribute {
        Some(_) => Err("an attribute that never closes, which hides the rest of the file"),
        None => Ok(out),
    }
}

/// One attribute, whitespace and string contents removed: `#[cfg(test)]`,
/// `#![doc=""]`.
fn judge_attribute(body: &str, out: &mut Scan) {
    let inner = body.trim_start_matches('#').trim_start_matches('!');
    let inner = inner.strip_prefix('[').and_then(|s| s.strip_suffix(']')).unwrap_or(inner);
    let words: Vec<&str> =
        inner.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).filter(|w| !w.is_empty()).collect();
    if words.iter().any(|w| *w == "test" || *w == "test_case") {
        out.test = true;
    }
    if matches!(words.first(), Some(&"cfg" | &"cfg_attr")) && inner != "cfg(test)" {
        out.conditions.push("a cfg other than cfg(test) can compile a test out of the host run");
    }
    if words.contains(&"ignore") {
        out.conditions.push("an ignored test runs nowhere");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    /// **Every userland test gates a merge.**
    #[test]
    fn every_userland_test_is_in_the_gate() {
        let survey = survey(&repo_root().join("userland")).expect("userland is readable");
        assert!(!survey.gated.is_empty(), "no userland crate holds a test, so this looked at nothing");
        assert!(
            survey.escapes.is_empty(),
            "`cargo run -- --ci host` runs none of these:\n  {}",
            survey.escapes.join("\n  ")
        );
    }

    /// The survey's judgment, on a tree built to hold each shape it has to tell
    /// apart.
    #[test]
    fn the_survey_gates_what_cargo_test_runs_and_names_the_rest() {
        let dir = toyos_tmpdir::TempDir::new("userlandhost");
        let put = |path: &str, text: &str| {
            let path = dir.join(path);
            fs::create_dir_all(path.parent().expect("a parent")).expect("make the fixture tree");
            fs::write(path, text).expect("write a fixture file");
        };
        let bare = "[package]\nname = \"x\"\nversion = \"0.1.0\"\n";
        let manifest = &format!("{bare}\n[lib]\ndoctest = false\n");
        let test = "#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {}\n}\n";
        let escape = "#[test]\nfn escapes() { panic!() }\n";

        put("loose.rs", escape);
        put("gated/Cargo.toml", manifest);
        put("gated/src/lib.rs", test);
        put("gated/src/slow.rs", "#[test]\n#[ignore]\nfn slow() {}\n");
        put("gated/build.rs", &format!("fn main() {{}}\n{escape}"));
        put("gated/examples/x.rs", &format!("fn main() {{}}\n{escape}"));
        put("gated/sub/Cargo.toml", manifest);
        put("gated/sub/src/lib.rs", test);
        put("testsonly/Cargo.toml", manifest);
        put("testsonly/src/lib.rs", "pub fn f() {}\n");
        put("testsonly/tests/t.rs", escape);
        put("plain/Cargo.toml", bare);
        put("plain/src/main.rs", "fn main() {}\n");
        put("plain/src/arch.rs", "#[cfg(target_arch = \"x86_64\")]\nfn a() {}\n");
        put("doctested/Cargo.toml", bare);
        put("doctested/src/lib.rs", "pub fn f() {}\n");
        put("switched/Cargo.toml", &format!("{bare}\n[lib]\ndoctest = false\ntest = false\n"));
        put("switched/src/lib.rs", test);
        put("switched/target/debug/x.rs", escape);

        let found = survey(&dir);
        assert_eq!(
            found,
            Ok(Survey {
                gated: vec!["gated".into(), "switched".into(), "testsonly".into()],
                escapes: vec![
                    "doctested/Cargo.toml: a library without [lib] doctest = false, whose \
                     doc-tests the gate does not read"
                        .into(),
                    "gated/build.rs: a test outside gated's src/ and tests/, which cargo test \
                     does not run"
                        .into(),
                    "gated/examples/x.rs: a test outside gated's src/ and tests/, which cargo \
                     test does not run"
                        .into(),
                    "gated/src/slow.rs: an ignored test runs nowhere".into(),
                    "gated/sub/src/lib.rs: a test in the nested crate gated/sub, which the gate \
                     does not discover"
                        .into(),
                    "loose.rs: a test in no crate".into(),
                    "switched/Cargo.toml: [lib] test = false leaves its tests unrun".into(),
                ],
            })
        );
    }

    #[test]
    fn a_test_attribute_is_a_word_and_not_a_substring() {
        let test = |text| scan(text).expect("closes").test;
        assert!(test("#[test]\nfn f() {}"));
        assert!(test("    #[cfg(test)]\nmod tests;"));
        assert!(test("#![cfg(test)]"));
        assert!(test("#[tokio::test]"));
        assert!(test("#[test_case]"));
        assert!(test("#[inline] #[test] fn f() {}"));
        assert!(!test("// #[test]\nlet test = 1;"));
        assert!(!test("#[derive(Debug)] struct Contest;"));
        // A feature is a string, and the words inside it are not attributes:
        // the kernel's `test-actuators` spelling holds no test, and is a
        // condition that could hide one.
        let actuators = scan("#[cfg(feature = \"test-actuators\")]\nmod m;").expect("closes");
        assert!(!actuators.test);
        assert!(!actuators.conditions.is_empty());
    }

    #[test]
    fn what_could_hide_a_test_is_a_condition() {
        let conditions = |text| scan(text).expect("closes").conditions;
        assert_eq!(conditions("#[cfg(test)]\nmod tests;"), Vec::<&str>::new());
        assert_eq!(conditions("#[cfg( test )]"), Vec::<&str>::new());
        let multi = scan("#[cfg(all(\n    test,\n    feature = \"x\",\n))]\nmod tests;").expect("closes");
        assert!(multi.test);
        assert_eq!(multi.conditions.len(), 1, "{multi:?}");
        assert!(!conditions("#[cfg(not(test))]").is_empty());
        assert!(!conditions("#[cfg(target_os = \"toyos\")]").is_empty());
        assert!(!conditions("#[test]\n#[ignore]\nfn f() {}").is_empty());
        assert!(!conditions("#[test] #[ignore]\nfn f() {}").is_empty());
        assert!(!conditions("#[cfg(test)] #[cfg(feature = \"slow\")] mod tests {").is_empty());
        assert!(conditions("#![feature(test)]").is_empty());
        assert!(scan("#[cfg(test)\nmod tests {}\n#[test]\nfn f() {}").is_err());
    }

    #[test]
    fn a_manifest_that_switches_tests_off_is_named() {
        let keys = |text: &str| switched_off(&text.parse().expect("TOML")).len();
        let base = "[package]\nname = \"x\"\n";
        assert_eq!(keys(base), 0);
        assert_eq!(keys(&format!("{base}[lib]\ndoctest = false\n")), 0);
        assert_eq!(keys(&format!("{base}autotests = false\n")), 1);
        assert_eq!(keys(&format!("{base}[[bin]]\nname = \"b\"\ntest = false\n")), 1);
        assert_eq!(keys(&format!("{base}[[test]]\nname = \"t\"\nharness = false\n")), 1);
        assert_eq!(keys(&format!("{base}[[test]]\nname = \"t\"\nrequired-features = [\"a\"]\n")), 1);
    }
}
