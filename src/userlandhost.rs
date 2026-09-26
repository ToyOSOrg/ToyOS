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
//! crate whose only `cfg` is `cfg(test)`, which names no feature and ignores no
//! test, and whose manifest switches off no target's tests. Everything else
//! test-shaped is an escape, and the gate reds on it by name. That covers a
//! nested crate's test, a test in `build.rs`, `examples/` or `benches/`, and any
//! doc-test (userland writes none: a fence that is not `text`, a `#[doc]`
//! attribute and a block doc comment are all refused).
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
    let mut gated = BTreeSet::new();
    let mut escapes = BTreeSet::new();
    // Refusals that bind only once the file's crate turns out to be gated.
    let mut conditional: Vec<(String, PathBuf, &'static str)> = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
        let scan = scan(&text);
        let at = rel(userland, file);
        for why in &scan.doc_tests {
            escapes.insert(format!("{at}: {why}"));
        }
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
    for crate_name in &gated {
        let manifest = userland.join(crate_name).join("Cargo.toml");
        let text =
            std::fs::read_to_string(&manifest).map_err(|e| format!("{}: {e}", manifest.display()))?;
        for key in switched_off(&text).map_err(|e| format!("{}: {e}", manifest.display()))? {
            escapes.insert(format!("{crate_name}/Cargo.toml: {key}"));
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
fn switched_off(manifest: &str) -> Result<Vec<String>, String> {
    let doc: toml::Value = manifest.parse().map_err(|e| format!("not TOML: {e}"))?;
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
    Ok(found)
}

/// What one source file holds, as far as the gate is concerned.
#[derive(Debug, Default, PartialEq, Eq)]
struct Scan {
    /// An attribute names `test` or `test_case` as a word.
    test: bool,
    /// Why a default `cargo test` might skip this crate's tests; binding only
    /// if the crate is gated.
    conditions: Vec<&'static str>,
    /// Doc-test shapes, which the gate refuses wherever they are.
    doc_tests: Vec<&'static str>,
}

/// Read `text` for tests, for what could hide one, and for doc-tests.
fn scan(text: &str) -> Scan {
    let mut out = Scan::default();
    let mut fence_open = false;
    let mut attribute: Option<(String, i32)> = None;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if attribute.is_none() {
            if let Some(doc) = trimmed.strip_prefix("///").or_else(|| trimmed.strip_prefix("//!")) {
                if let Some(info) = doc.trim().strip_prefix("```") {
                    if !fence_open && info.trim() != "text" {
                        out.doc_tests.push("a doc fence that is not `text` is a doc-test");
                    }
                    fence_open = !fence_open;
                }
                continue;
            }
            if trimmed.starts_with("/**") || trimmed.starts_with("/*!") {
                out.doc_tests.push("a block doc comment, whose fences this does not read");
                continue;
            }
            if !(trimmed.starts_with("#[") || trimmed.starts_with("#![")) {
                continue;
            }
            attribute = Some((String::new(), 0));
        }
        let (body, depth) = attribute.as_mut().expect("inside an attribute");
        let mut in_string = false;
        let mut escaped = false;
        let mut closed = false;
        for c in trimmed.chars() {
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
                closed = true;
                break;
            }
        }
        if closed {
            let (body, _) = attribute.take().expect("inside an attribute");
            judge_attribute(&body, &mut out);
        } else {
            attribute.as_mut().expect("inside an attribute").0.push(' ');
        }
    }
    out
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
    match words.first() {
        Some(&"doc") => out.doc_tests.push("a #[doc] attribute, which can carry a doc-test"),
        Some(&"cfg" | &"cfg_attr") if inner != "cfg(test)" => {
            out.conditions.push("a cfg other than cfg(test) can compile a test out of the host run")
        }
        _ => {}
    }
    if words.contains(&"feature") {
        out.conditions.push("a feature attribute can compile a test out of a default cargo test");
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
        let dir = std::env::temp_dir().join(format!("toyos-userlandhost-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let put = |path: &str, text: &str| {
            let path = dir.join(path);
            fs::create_dir_all(path.parent().expect("a parent")).expect("make the fixture tree");
            fs::write(path, text).expect("write a fixture file");
        };
        let manifest = "[package]\nname = \"x\"\nversion = \"0.1.0\"\n";
        let test = "#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {}\n}\n";
        let escape = "#[test]\nfn escapes() { panic!() }\n";

        put("gated/Cargo.toml", manifest);
        put("gated/src/lib.rs", test);
        put("gated/build.rs", &format!("fn main() {{}}\n{escape}"));
        put("gated/examples/x.rs", &format!("fn main() {{}}\n{escape}"));
        put("gated/sub/Cargo.toml", manifest);
        put("gated/sub/src/lib.rs", test);
        put("testsonly/Cargo.toml", manifest);
        put("testsonly/src/lib.rs", "pub fn f() {}\n");
        put("testsonly/tests/t.rs", escape);
        put("plain/Cargo.toml", manifest);
        put("plain/src/main.rs", "fn main() {}\n");
        put("switched/Cargo.toml", &format!("{manifest}\n[lib]\ntest = false\n"));
        put("switched/src/lib.rs", test);
        put("switched/target/debug/x.rs", escape);

        let found = survey(&dir);
        fs::remove_dir_all(&dir).ok();
        assert_eq!(
            found,
            Ok(Survey {
                gated: vec!["gated".into(), "switched".into(), "testsonly".into()],
                escapes: vec![
                    "gated/build.rs: a test outside gated's src/ and tests/, which cargo test \
                     does not run"
                        .into(),
                    "gated/examples/x.rs: a test outside gated's src/ and tests/, which cargo \
                     test does not run"
                        .into(),
                    "gated/sub/src/lib.rs: a test in the nested crate gated/sub, which the gate \
                     does not discover"
                        .into(),
                    "switched/Cargo.toml: [lib] test = false leaves its tests unrun".into(),
                ],
            })
        );
    }

    #[test]
    fn a_test_attribute_is_a_word_and_not_a_substring() {
        assert!(scan("#[test]\nfn f() {}").test);
        assert!(scan("    #[cfg(test)]\nmod tests;").test);
        assert!(scan("#![cfg(test)]").test);
        assert!(scan("#[tokio::test]").test);
        assert!(scan("#[test_case]").test);
        assert!(!scan("// #[test]\nlet test = 1;").test);
        assert!(!scan("#[derive(Debug)] struct Contest;").test);
        // A feature is a string, and the words inside it are not attributes:
        // the kernel's `test-actuators` spelling holds no test, and is a
        // condition that could hide one.
        let actuators = scan("#[cfg(feature = \"test-actuators\")]\nmod m;");
        assert!(!actuators.test);
        assert!(!actuators.conditions.is_empty());
    }

    #[test]
    fn what_could_hide_a_test_is_a_condition() {
        assert_eq!(scan("#[cfg(test)]\nmod tests;").conditions, Vec::<&str>::new());
        assert_eq!(scan("#[cfg( test )]").conditions, Vec::<&str>::new());
        let multi = scan("#[cfg(all(\n    test,\n    feature = \"x\",\n))]\nmod tests;");
        assert!(multi.test);
        assert_eq!(multi.conditions.len(), 2, "{multi:?}");
        assert!(!scan("#[cfg(not(test))]").conditions.is_empty());
        assert!(!scan("#[cfg(target_os = \"toyos\")]").conditions.is_empty());
        assert!(!scan("#[test]\n#[ignore]\nfn f() {}").conditions.is_empty());
    }

    #[test]
    fn a_doc_test_is_refused_and_a_text_fence_is_not() {
        assert!(scan("//! ```text\n//! a -> b\n//! ```\n").doc_tests.is_empty());
        assert_eq!(scan("/// ```\n/// f();\n/// ```\n").doc_tests.len(), 1);
        assert_eq!(scan("//! ```rust,no_run\n//! ```\n").doc_tests.len(), 1);
        assert_eq!(scan("#![doc = include_str!(\"README.md\")]").doc_tests.len(), 1);
        assert_eq!(scan("/** a */").doc_tests.len(), 1);
    }

    #[test]
    fn a_manifest_that_switches_tests_off_is_named() {
        let base = "[package]\nname = \"x\"\n";
        assert_eq!(switched_off(base), Ok(vec![]));
        assert_eq!(switched_off(&format!("{base}[lib]\ndoctest = false\n")), Ok(vec![]));
        assert_eq!(switched_off(&format!("{base}autotests = false\n")).map(|v| v.len()), Ok(1));
        assert_eq!(switched_off(&format!("{base}[[bin]]\nname = \"b\"\ntest = false\n")).map(|v| v.len()), Ok(1));
        assert_eq!(switched_off(&format!("{base}[[test]]\nname = \"t\"\nharness = false\n")).map(|v| v.len()), Ok(1));
        assert_eq!(
            switched_off(&format!("{base}[[test]]\nname = \"t\"\nrequired-features = [\"a\"]\n")).map(|v| v.len()),
            Ok(1)
        );
    }
}
