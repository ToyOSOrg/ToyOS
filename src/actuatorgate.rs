//! An actuator's doc may name its reader, and a name that resolves to nothing
//! is the bug this refuses.
//!
//! `log-unbracketed-reserve`'s doc once named a test that never existed, for as
//! long as the actuator did, and the only thing that found it was a person
//! reading the file. So a backticked snake_case identifier in an actuator's doc
//! must resolve: to an item `kernel/src` declares — the doc is describing the
//! kernel — or to a name the test tree carries, the doc naming its reader. One
//! that resolves to neither is a dead pointer, refused here, the same shape of
//! gate `redlist` runs over the issue paths a source comment cites.
//!
//! Read by nothing but its own tests, so it is not compiled into the build.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// One actuator: its wire name and the doc comment that precedes it, joined.
struct Actuator {
    name: String,
    doc: String,
}

/// The actuators `kernel/src/actuator.rs` declares, each with its doc.
///
/// The `actuators!` block is [`crate::build::declared_actuators`]' shape; this
/// keeps the `///` lines that one drops.
fn actuators(source: &str) -> Vec<Actuator> {
    let body = source
        .split_once("\nactuators! {\n")
        .expect("kernel/src/actuator.rs has no `actuators!` block")
        .1;
    let body = body.split_once("\n}\n").expect("the `actuators!` block does not end").0;
    let mut out = Vec::new();
    let mut doc = String::new();
    for line in body.lines().map(str::trim) {
        if let Some(text) = line.strip_prefix("///") {
            doc.push_str(text.trim());
            doc.push('\n');
        } else if let Some((lhs, _)) = line.split_once(" = \"") {
            out.push(Actuator { name: lhs.trim().to_string(), doc: std::mem::take(&mut doc) });
        } else if !line.is_empty() {
            doc.clear();
        }
    }
    out
}

/// The backticked tokens in `doc` that read as a bare test name: one lowercase
/// `[a-z][a-z0-9_]*` identifier carrying at least one `_`, so a `::` path, an
/// upper-case type and a multi-word phrase are all left as prose.
fn named_identifiers(doc: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = doc;
    while let Some(open) = rest.find('`') {
        rest = &rest[open + 1..];
        let Some(close) = rest.find('`') else { break };
        let token = &rest[..close];
        rest = &rest[close + 1..];
        if is_bare_test_name(token) {
            out.push(token.to_string());
        }
    }
    out
}

/// Whether `token` is a bare lowercase snake_case identifier.
fn is_bare_test_name(token: &str) -> bool {
    token.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
        && token.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        && token.contains('_')
}

/// The identifiers a doc may name: every `[A-Za-z0-9_]` run in `corpus`.
fn words(corpus: &str) -> BTreeSet<String> {
    corpus
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// Every dead pointer, one line each. Pure over its two inputs so the negative
/// control can stage an actuator block and a corpus that are not on disk.
fn refusals(source: &str, resolvable: &BTreeSet<String>) -> Vec<String> {
    let mut bad = Vec::new();
    for actuator in actuators(source) {
        for id in named_identifiers(&actuator.doc) {
            if !resolvable.contains(&id) {
                bad.push(format!(
                    "actuator `{}`'s doc names `{id}`, which is neither an item kernel/src \
                     declares nor a name the test tree carries — a reader that does not exist",
                    actuator.name
                ));
            }
        }
    }
    bad
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    fn collect(dir: &Path, exts: &[&str], out: &mut String) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect(&path, exts, out);
            } else if path.extension().is_some_and(|e| exts.iter().any(|x| e == *x)) {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    out.push_str(&text);
                    out.push('\n');
                }
            }
        }
    }

    /// Every identifier `kernel/src` and the test tree carry.
    fn resolvable(root: &Path) -> BTreeSet<String> {
        let mut corpus = String::new();
        collect(&root.join("kernel/src"), &["rs"], &mut corpus);
        collect(&root.join("tests"), &["rs", "c"], &mut corpus);
        words(&corpus)
    }

    #[test]
    fn no_actuator_doc_names_a_reader_that_does_not_exist() {
        let root = repo_root();
        let source = std::fs::read_to_string(root.join("kernel/src/actuator.rs")).unwrap();
        let acts = actuators(&source);
        assert!(acts.len() > 50, "the actuator parse found only {}", acts.len());
        assert!(
            acts.iter().any(|a| !a.doc.trim().is_empty()),
            "no actuator doc was extracted — the parser, not the tree, is what greened this"
        );
        let bad = refusals(&source, &resolvable(&root));
        assert!(
            bad.is_empty(),
            "an actuator naming its reader must name a real one:\n  {}",
            bad.join("\n  ")
        );
    }

    /// Teeth: a staged block whose second actuator names a ghost is refused, and
    /// the first — naming a kernel item and a real test — is not.
    #[test]
    fn the_gate_refuses_a_doc_that_names_a_ghost() {
        let source = concat!(
            "\nactuators! {\n",
            "    /// Reads `parse_config`, checked by `control_regs_negative`.\n",
            "    real_reader = \"real-reader\";\n",
            "    /// Checked by `this_reader_never_existed`.\n",
            "    dangling = \"dangling\";\n",
            "}\n",
        );
        let resolvable: BTreeSet<String> =
            ["parse_config", "control_regs_negative"].into_iter().map(str::to_string).collect();
        let bad = refusals(source, &resolvable);
        assert_eq!(bad.len(), 1, "{bad:?}");
        assert!(bad[0].contains("this_reader_never_existed") && bad[0].contains("dangling"));
    }

    #[test]
    fn the_name_filter_takes_bare_snake_case_and_leaves_prose() {
        assert!(is_bare_test_name("log_migration_storm"));
        assert!(is_bare_test_name("parse_config"));
        assert!(!is_bare_test_name("klogd"));
        assert!(!is_bare_test_name("SYS_FSYNC"));
        assert!(!is_bare_test_name("Source::Log"));
        assert!(!is_bare_test_name("mov cr0"));
        assert_eq!(
            named_identifiers("names `log_migration_storm`, `mm::init` and `iod`"),
            ["log_migration_storm"]
        );
    }
}
