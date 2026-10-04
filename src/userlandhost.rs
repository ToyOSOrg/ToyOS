//! Which userland crates `cargo run -- --ci host` tests, every userland test it
//! would run nowhere, and what every program the images ship declares about the
//! hosts.
//!
//! [`programs`] reads each program [`crate::build::shipped`] names, and its own
//! manifest. One that declares nothing is an app, and builds for Linux, macOS
//! and Windows. One whose job exists only on ToyOS names which of the two cases
//! it is, and why: `exempt.owns` the ToyOS devices or kernel objects it owns,
//! or `exempt.manages` the part of ToyOS it manages.
//!
//! ```toml
//! [package.metadata.toyos.host]
//! exempt.owns = "the NVMe controller ToyOS claims for it"
//! ```
//!
//! An app that does not build for a host yet names the host, and the open issue
//! that records it and names the app. The gate does not attempt it there:
//!
//! ```toml
//! [package.metadata.toyos.host]
//! fails = ["windows"]
//! issue = "issues/build/<slug>.md"
//! ```
//!
//! A host's apps are built where the gate runs on its triple ([`Os::triple`]),
//! and checked against that triple elsewhere. A check links nothing and runs
//! build scripts on the host that checks, and a declared failure is attempted on
//! no host, so one that outlives its fix goes unseen
//! (`issues/build/a-hosts-apps-are-judged-on-other-hosts-runners.md`). An app
//! whose work a `cfg` compiles out of a host checks green there: only review
//! holds that (`.claude/agents/reviewer.md`, Hosts).
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
//! crate is known to run. That is an attribute naming `test` or `test_case`, in
//! a file under the `src/` or `tests/` of the crate nearest it, in a crate whose
//! only `cfg` is `cfg(test)`, which ignores no test, and whose manifest switches
//! off no target's tests.
//!
//! Doc-tests are not read: a library crate that leaves `doctest` on is refused,
//! because the `toyos` toolchain builds no rustdoc.
//!
//! A test in a `src/` file that no `mod` reaches is gated and never compiled.
//! That escape is not closed here.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::build::Features;

/// What [`survey`] found under one `userland/` directory.
#[derive(Debug, PartialEq, Eq)]
pub struct Survey {
    /// The crates `host` runs, by directory under `userland/`, sorted.
    pub gated: Vec<String>,
    /// Every test the gate would not run, each as `<path under userland>: why`,
    /// sorted. `host` is red while this is not empty.
    pub escapes: Vec<String>,
}

/// Every crate under `userland` that holds a test, and every test that no
/// `cargo test` in one of them runs.
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
        if !(inside.starts_with("src/") || inside.starts_with("tests/")) {
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

/// A host an app builds for, as a manifest spells it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Linux,
    Macos,
    Windows,
}

impl Os {
    pub const ALL: [Os; 3] = [Os::Linux, Os::Macos, Os::Windows];

    pub fn name(self) -> &'static str {
        match self {
            Os::Linux => "linux",
            Os::Macos => "macos",
            Os::Windows => "windows",
        }
    }

    /// The triple the gate judges this host's apps for.
    pub fn triple(self) -> &'static str {
        match self {
            Os::Linux => "x86_64-unknown-linux-gnu",
            Os::Macos => "aarch64-apple-darwin",
            Os::Windows => "x86_64-pc-windows-msvc",
        }
    }

    fn named(name: &str) -> Option<Os> {
        Os::ALL.into_iter().find(|os| os.name() == name)
    }
}

/// What a program's manifest declares about the hosts.
#[derive(Debug, PartialEq, Eq)]
pub enum Host {
    /// An app: it builds for every host but these, which an open issue records.
    App(Vec<Os>),
    /// Its job exists only on ToyOS.
    Exempt,
}

/// One program the images ship.
#[derive(Debug)]
pub struct Program {
    /// Its directory under the repository.
    pub dir: String,
    /// What the image builds it with.
    pub features: Features,
    pub host: Host,
}

/// Every program [`crate::build::shipped`] names, and what its manifest
/// declares about the hosts.
pub fn programs(root: &Path) -> Result<Vec<Program>, String> {
    let mut found = Vec::new();
    for (dir, features) in crate::build::shipped(root)?.programs {
        let at = rel(root, &dir);
        let text = std::fs::read_to_string(dir.join("Cargo.toml"))
            .map_err(|e| format!("{at}/Cargo.toml: {e}"))?;
        let manifest: toml::Value =
            text.parse().map_err(|e| format!("{at}/Cargo.toml: not TOML: {e}"))?;
        let host = declared(&manifest, root).map_err(|why| format!("{at}/Cargo.toml: {why}"))?;
        found.push(Program { dir: at, features, host });
    }
    Ok(found)
}

/// `[package.metadata.toyos.host]`, read whole: anything it does not know is
/// refused, because a misspelt key would make an exempt program an app or a
/// failing one quiet.
fn declared(manifest: &toml::Value, root: &Path) -> Result<Host, String> {
    let package = manifest.get("package").ok_or("no [package]")?;
    let toyos = package.get("metadata").and_then(|m| m.get("toyos"));
    let Some(host) = toyos.and_then(|t| t.get("host")) else {
        return Ok(Host::App(Vec::new()));
    };
    let host = host.as_table().ok_or("[package.metadata.toyos.host] is not a table")?;
    if let Some(key) = host.keys().find(|k| !["exempt", "fails", "issue"].contains(&k.as_str())) {
        return Err(format!("[package.metadata.toyos.host] declares `{key}`, which nothing reads"));
    }
    match (host.get("exempt"), host.get("fails"), host.get("issue")) {
        (Some(exempt), None, None) => {
            let case = exempt.as_table().filter(|t| t.len() == 1).and_then(|t| t.iter().next());
            match case {
                Some((case, why))
                    if ["owns", "manages"].contains(&case.as_str())
                        && why.as_str().is_some_and(|why| !why.trim().is_empty()) =>
                {
                    Ok(Host::Exempt)
                }
                _ => Err("`exempt` is `exempt.owns`, the ToyOS devices or kernel objects it owns, \
                          or `exempt.manages`, the part of ToyOS it manages"
                    .into()),
            }
        }
        (None, Some(fails), Some(issue)) => {
            let mut on = Vec::new();
            for name in fails.as_array().into_iter().flatten() {
                let os = name.as_str().and_then(Os::named).ok_or_else(|| {
                    format!("`fails` names {name}, which is none of linux, macos and windows")
                })?;
                if on.contains(&os) {
                    return Err(format!("`fails` names {} twice", os.name()));
                }
                on.push(os);
            }
            if on.is_empty() {
                return Err("`fails` names no host".into());
            }
            let issue = issue.as_str().ok_or("`issue` is not a path")?;
            let app = package.get("name").and_then(toml::Value::as_str).ok_or("no [package] name")?;
            owed(root, issue, app)?;
            Ok(Host::App(on))
        }
        _ => Err("[package.metadata.toyos.host] declares `exempt` alone, or `fails` with its \
                  `issue`"
            .into()),
    }
}

/// `issue` is `issues/<area>/<slug>.md`, work still owed, and names `app` in
/// backticks.
fn owed(root: &Path, issue: &str, app: &str) -> Result<(), String> {
    let word = |s: &str| {
        !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    };
    let shaped = match issue.split('/').collect::<Vec<_>>()[..] {
        ["issues", area, slug] => word(area) && slug.strip_suffix(".md").is_some_and(word),
        _ => false,
    };
    if !shaped {
        return Err(format!("`issue` is {issue:?}, which is no issues/<area>/<slug>.md"));
    }
    let text = std::fs::read_to_string(root.join(issue))
        .map_err(|e| format!("`issue` is {issue}, which does not open: {e}"))?;
    let status = text
        .strip_prefix("---\n")
        .and_then(|rest| rest.split("\n---\n").next())
        .into_iter()
        .flat_map(str::lines)
        .find_map(|line| line.strip_prefix("status: "));
    if !matches!(status, Some("open" | "assigned")) {
        return Err(format!("{issue} is no work still owed: its status is {status:?}"));
    }
    if !text.contains(&format!("`{app}`")) {
        return Err(format!("{issue} does not name `{app}`, so the set it records is short"));
    }
    Ok(())
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
                gated: vec!["gated".into(), "gated/sub".into(), "switched".into(), "testsonly".into()],
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
                    "loose.rs: a test in no crate".into(),
                    "switched/Cargo.toml: [lib] test = false leaves its tests unrun".into(),
                ],
            })
        );
    }

    /// **Every program the images ship is an app or says why it is not**, the
    /// ones outside the userland workspace too.
    #[test]
    fn every_program_the_images_ship_declares_what_it_is_to_a_host() {
        let root = repo_root();
        let programs = programs(&root).expect("every declaration reads");
        let shipped = crate::build::shipped(&root).expect("the modes' configs").programs;
        let dirs: BTreeSet<PathBuf> = programs.iter().map(|p| root.join(&p.dir)).collect();
        assert_eq!(dirs, shipped.into_iter().map(|(dir, _)| dir).collect());
        assert!(programs.iter().any(|p| p.host == Host::App(Vec::new())), "no app: {programs:?}");
        assert!(programs.iter().any(|p| p.host == Host::Exempt), "no exemption: {programs:?}");
    }

    #[test]
    fn a_host_declaration_is_read_whole_and_refused_by_name() {
        let dir = toyos_tmpdir::TempDir::new("userlandhost-declared");
        let put = |path: &str, status: &str| {
            let path = dir.join(path);
            fs::create_dir_all(path.parent().expect("a parent")).expect("make the fixture tree");
            let text = format!("---\nstatus: {status}\nkind: defect\n---\n\n| `calc` | 101 |\n");
            fs::write(path, text).expect("write a fixture issue");
        };
        put("issues/build/x.md", "open");
        put("issues/build/held.md", "assigned");
        put("issues/build/asked.md", "owner");
        put("issues/README.md", "open");
        put("notes.md", "open");
        let read = |host: &str| {
            let manifest = format!("[package]\nname = \"calc\"\n{host}");
            declared(&manifest.parse().expect("TOML"), &dir)
        };
        let table = "[package.metadata.toyos.host]\n";
        let issue = "issue = \"issues/build/x.md\"\n";
        assert_eq!(read(""), Ok(Host::App(Vec::new())));
        assert_eq!(read("[package.metadata.toyos]\nother = 1\n"), Ok(Host::App(Vec::new())));
        for case in ["owns", "manages"] {
            assert_eq!(read(&format!("{table}exempt.{case} = \"a panel\"\n")), Ok(Host::Exempt));
        }
        assert_eq!(
            read(&format!("{table}fails = [\"windows\", \"linux\"]\n{issue}")),
            Ok(Host::App(vec![Os::Windows, Os::Linux]))
        );
        assert_eq!(
            read(&format!("{table}fails = [\"linux\"]\nissue = \"issues/build/held.md\"\n")),
            Ok(Host::App(vec![Os::Linux]))
        );
        for refused in [
            format!("{table}exempt = \"a panel\"\n"),
            format!("{table}exempt.serves = \"a port\"\n"),
            format!("{table}exempt.owns = \" \"\n"),
            format!("{table}exempt.owns = 1\n"),
            format!("{table}exempt = {{}}\n"),
            format!("{table}exempt = {{ owns = \"a panel\", manages = \"a slot\" }}\n"),
            format!("{table}exempt.owns = \"a panel\"\nfails = [\"linux\"]\n{issue}"),
            format!("{table}fails = [\"linux\"]\n"),
            format!("{table}{issue}"),
            format!("{table}fails = []\n{issue}"),
            format!("{table}fails = [\"freebsd\"]\n{issue}"),
            format!("{table}fails = [\"linux\", \"linux\"]\n{issue}"),
            format!("{table}fails = \"linux\"\n{issue}"),
            format!("{table}fails = [\"linux\"]\nissue = \"notes.md\"\n"),
            format!("{table}fails = [\"linux\"]\nissue = \"issues/README.md\"\n"),
            format!("{table}fails = [\"linux\"]\nissue = \"issues/../notes.md\"\n"),
            format!("{table}fails = [\"linux\"]\nissue = \"issues/build/asked.md\"\n"),
            format!("{table}fails = [\"linux\"]\nissue = \"issues/build/gone.md\"\n"),
            format!("{table}exmept.owns = \"a panel\"\n"),
            format!("{table}exempt.owns = \"a panel\"\nnote = \"it may grow\"\n"),
            "[package.metadata.toyos]\nhost = \"exempt\"\n".to_string(),
        ] {
            assert!(read(&refused).is_err(), "read {refused:?}");
        }
        let unnamed = format!("[package]\nname = \"snake\"\n{table}fails = [\"linux\"]\n{issue}");
        let refusal = declared(&unnamed.parse().expect("TOML"), &dir).unwrap_err();
        assert!(refusal.contains("does not name `snake`"), "{refusal}");
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
