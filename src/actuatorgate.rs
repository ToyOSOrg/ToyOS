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
//!
//! The second gate here holds actuator *state*: a static an actuator-guarded
//! site writes may be touched only under a guard (or inside a
//! `#[cfg(...-actuators)]` item) — [`unguarded_coupled_touches`]'s doc has
//! the class and its boundary.

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

/// `text` with comments and string/char-literal contents blanked to spaces,
/// newlines kept — what makes brace counting over kernel source sound, since
/// every `log!` format string is full of braces.
fn code_only(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = vec![b' '; b.len()];
    let mut i = 0;
    let keep_newlines = |out: &mut Vec<u8>, from: usize, to: usize| {
        for (j, &c) in b[from..to].iter().enumerate() {
            if c == b'\n' {
                out[from + j] = b'\n';
            }
        }
    };
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                let end = text[i..].find('\n').map_or(b.len(), |p| i + p);
                keep_newlines(&mut out, i, end);
                i = end;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let end = text[i + 2..].find("*/").map_or(b.len(), |p| i + 2 + p + 2);
                keep_newlines(&mut out, i, end);
                i = end;
            }
            b'"' => {
                let mut j = i + 1;
                while j < b.len() && b[j] != b'"' {
                    j += if b[j] == b'\\' { 2 } else { 1 };
                }
                keep_newlines(&mut out, i, j.min(b.len()));
                i = (j + 1).min(b.len());
            }
            // A char literal, told from a lifetime by closing within two.
            b'\'' if matches!(
                (b.get(i + 1), b.get(i + 2)),
                (Some(b'\\'), _) | (_, Some(b'\''))
            ) =>
            {
                let mut j = i + 1;
                while j < b.len() && b[j] != b'\'' {
                    j += if b[j] == b'\\' { 2 } else { 1 };
                }
                i = (j + 1).min(b.len());
            }
            c => {
                out[i] = c;
                i += 1;
            }
        }
    }
    String::from_utf8(out).expect("blanking ASCII bytes keeps UTF-8 valid")
}

/// Every byte range an `actuator::…` mention guards in `clean` code: the
/// enclosing statement (back to the previous `;`/`{`/`}`), and — where the
/// expression goes on to open a block — that whole block.
fn guard_spans(clean: &str) -> Vec<(usize, usize)> {
    let bytes = clean.as_bytes();
    let mut spans = Vec::new();
    let mut from = 0;
    while let Some(at) = clean[from..].find("actuator::").map(|p| p + from) {
        from = at + "actuator::".len();
        let start = clean[..at].rfind([';', '{', '}']).map_or(0, |p| p + 1);
        let mut i = from;
        let mut depth: i32 = 0;
        let end = loop {
            let Some(&b) = bytes.get(i) else { break i };
            match b {
                b'(' | b'[' => depth += 1,
                b')' | b']' if depth > 0 => depth -= 1,
                b')' | b']' | b';' | b',' => break i + 1,
                // A block's close: the mention was an expression tail.
                b'}' => break i,
                b'{' if depth == 0 => {
                    let mut braces = 1;
                    let mut j = i + 1;
                    while j < bytes.len() && braces > 0 {
                        match bytes[j] {
                            b'{' => braces += 1,
                            b'}' => braces -= 1,
                            _ => {}
                        }
                        j += 1;
                    }
                    break j;
                }
                _ => {}
            }
            i += 1;
        };
        spans.push((start, end));
    }
    spans
}

/// Every atomic operation on a SCREAMING_CASE static in `clean` code, as
/// (byte position, name, is a write).
fn atomic_touches(clean: &str) -> Vec<(usize, String, bool)> {
    const WRITES: &[&str] = &[".store(", ".fetch_", ".swap(", ".compare_exchange"];
    let mut out = Vec::new();
    for op in WRITES.iter().chain(std::iter::once(&".load(")) {
        let mut from = 0;
        while let Some(at) = clean[from..].find(op).map(|p| p + from) {
            from = at + op.len();
            let head = clean.as_bytes();
            let mut s = at;
            while s > 0 && matches!(head[s - 1], b'A'..=b'Z' | b'0'..=b'9' | b'_') {
                s -= 1;
            }
            let name = &clean[s..at];
            if name.len() >= 2 && name.as_bytes()[0].is_ascii_uppercase() {
                out.push((s, name.to_string(), *op != ".load("));
            }
        }
    }
    out.sort();
    out
}

/// The byte ranges of items under an actuators `#[cfg(...)]` — compiled into
/// no shipping kernel, so the property below is not theirs. `not(...)` forms
/// mark shipping-only code and stay analysed. Attributes are found on `raw`
/// (the feature name is a string literal `clean` blanks); extents are walked
/// on `clean`, whose braces are code's alone.
fn cfg_exempt_ranges(raw: &str, clean: &str) -> Vec<(usize, usize)> {
    let bytes = clean.as_bytes();
    let mut out = Vec::new();
    let mut pos = 0;
    for line in raw.lines() {
        let end = pos + line.len();
        let t = line.trim_start();
        if t.starts_with("#[cfg(") && t.contains("-actuators\"") && !t.contains("not(") {
            let mut i = end;
            let mut depth: i32 = 0;
            let close = loop {
                let Some(&b) = bytes.get(i) else { break i };
                match b {
                    b'(' | b'[' => depth += 1,
                    b')' | b']' if depth > 0 => depth -= 1,
                    b';' if depth == 0 => break i + 1,
                    b'{' if depth == 0 => {
                        let mut braces = 1;
                        let mut j = i + 1;
                        while j < bytes.len() && braces > 0 {
                            match bytes[j] {
                                b'{' => braces += 1,
                                b'}' => braces -= 1,
                                _ => {}
                            }
                            j += 1;
                        }
                        break j;
                    }
                    _ => {}
                }
                i += 1;
            };
            out.push((pos, close));
        }
        pos = end + 1;
    }
    out
}

/// The class that put the i8042 fault flag's load on every shipping status
/// read: a static an actuator-guarded site *writes* is actuator state, so any
/// touch of it outside every guard — and outside `#[cfg(...-actuators)]`
/// items, which no shipping kernel compiles — runs with nothing armed, on the
/// shipping kernel included, where the accessor is `const false` and the
/// guarded arm folds away. Writes and not reads decide the coupling: shipping
/// state an actuator merely consults (the i8042 health machine, the fat32
/// drain flag) is written by ordinary code everywhere, and a read-coupling
/// rule called every one of those a refusal.
fn unguarded_coupled_touches(files: &[(String, String)]) -> Vec<String> {
    struct File<'a> {
        path: &'a str,
        clean: String,
        spans: Vec<(usize, usize)>,
        exempt: Vec<(usize, usize)>,
    }
    let analysed: Vec<File> = files
        .iter()
        .map(|(path, text)| {
            let clean = code_only(text);
            let spans = guard_spans(&clean);
            let exempt = cfg_exempt_ranges(text, &clean);
            File { path, clean, spans, exempt }
        })
        .collect();
    let within = |ranges: &[(usize, usize)], pos: usize| {
        ranges.iter().any(|(s, e)| (*s..*e).contains(&pos))
    };
    let mut coupled: BTreeSet<String> = BTreeSet::new();
    for file in &analysed {
        for (pos, name, write) in atomic_touches(&file.clean) {
            if write && within(&file.spans, pos) && !within(&file.exempt, pos) {
                coupled.insert(name);
            }
        }
    }
    let mut bad = Vec::new();
    for file in &analysed {
        for (pos, name, _) in atomic_touches(&file.clean) {
            if coupled.contains(&name)
                && !within(&file.spans, pos)
                && !within(&file.exempt, pos)
            {
                let line = file.clean[..pos].matches('\n').count() + 1;
                bad.push(format!(
                    "{}:{line}: `{name}` is actuator state — an actuator-guarded site writes \
                     it — and this touch stands outside every `actuator::` guard, so it runs \
                     with nothing armed, on the shipping kernel included",
                    file.path
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

    /// Every kernel source beside `actuator.rs` itself, whose `ARMED` words
    /// are the mechanism rather than a use of it.
    fn kernel_sources(root: &Path) -> Vec<(String, String)> {
        fn walk(dir: &Path, out: &mut Vec<(String, String)>) {
            let Ok(entries) = std::fs::read_dir(dir) else { return };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs")
                    && path.file_name().is_some_and(|n| n != "actuator.rs")
                {
                    let text = std::fs::read_to_string(&path).expect("kernel source reads");
                    out.push((path.display().to_string(), text));
                }
            }
        }
        let mut out = Vec::new();
        walk(&root.join("kernel/src"), &mut out);
        out.sort();
        out
    }

    #[test]
    fn actuator_state_is_touched_only_under_an_actuator_guard() {
        let files = kernel_sources(&repo_root());
        assert!(files.len() > 50, "the kernel walk found only {} sources", files.len());
        let bad = unguarded_coupled_touches(&files);
        assert!(
            bad.is_empty(),
            "an actuator's static reached the shipping kernel's path:\n  {}",
            bad.join("\n  ")
        );
    }

    /// Teeth, against the recorded defect's own shape and across files the
    /// way it happened: the i8042 fault flag was stored under its actuator
    /// and loaded on every shipping status read. The bare load is refused;
    /// put under the accessor's own condition, the same sources pass.
    #[test]
    fn the_gate_refuses_the_unguarded_load_that_shipped() {
        let armer = concat!(
            "fn init() {\n",
            "    if crate::actuator::i8042_fault() {\n",
            "        FAULT.store(true, Ordering::Relaxed);\n",
            "        log!(\"i8042: fault {} armed\");\n",
            "    }\n",
            "}\n",
        );
        let broken = "fn buffer_full() -> bool { FAULT.load(Ordering::Relaxed) }\n";
        let fixed = concat!(
            "fn buffer_full() -> bool {\n",
            "    crate::actuator::i8042_fault() && FAULT.load(Ordering::Relaxed)\n",
            "}\n",
        );
        let bad = unguarded_coupled_touches(&[
            ("armer.rs".to_string(), armer.to_string()),
            ("reader.rs".to_string(), broken.to_string()),
        ]);
        assert_eq!(bad.len(), 1, "{bad:?}");
        assert!(bad[0].contains("reader.rs:1") && bad[0].contains("FAULT"), "{bad:?}");
        let good = unguarded_coupled_touches(&[
            ("armer.rs".to_string(), armer.to_string()),
            ("reader.rs".to_string(), fixed.to_string()),
        ]);
        assert!(good.is_empty(), "{good:?}");
    }

    /// The regression `BOOT_SCAN_DONE` recorded stays caught from today's
    /// tree: its store is guarded now, so an extra unconditional store —
    /// exactly the line that shipped — is a refusal, while shipping state an
    /// actuator merely reads stays free, and a `#[cfg(...-actuators)]` item's
    /// touches are the actuator kernel's own business.
    #[test]
    fn the_gate_takes_writes_as_the_coupling_and_exempts_cfg_items() {
        let guarded = concat!(
            "fn scan() {\n",
            "    if crate::actuator::xhci_slow_storage_connect() {\n",
            "        BOOT_SCAN_DONE.store(true, Ordering::Relaxed);\n",
            "    }\n",
            "    HEALTH.load(Ordering::Relaxed);\n",
            "}\n",
        );
        let regressed = format!("{guarded}fn boot() {{ BOOT_SCAN_DONE.store(true, O); }}\n");
        let bad = unguarded_coupled_touches(&[("xhci.rs".to_string(), regressed)]);
        assert_eq!(bad.len(), 1, "{bad:?}");
        assert!(bad[0].contains("BOOT_SCAN_DONE") && bad[0].contains("xhci.rs:7"), "{bad:?}");

        let consulted = format!(
            "{guarded}fn tick() {{ HEALTH.store(1, O); HEALTH.load(O); }}\n"
        );
        let good = unguarded_coupled_touches(&[("xhci.rs".to_string(), consulted)]);
        assert!(good.is_empty(), "reads do not couple: {good:?}");

        let cfgd = concat!(
            "#[cfg(feature = \"boot-actuators\")]\n",
            "mod armed {\n",
            "    fn arm() {\n",
            "        if crate::actuator::usb_transport_break() { UNSPENT.store(true, O); }\n",
            "    }\n",
            "    fn take() -> bool { UNSPENT.swap(false, O) }\n",
            "}\n",
        );
        let good = unguarded_coupled_touches(&[("msc.rs".to_string(), cfgd.to_string())]);
        assert!(good.is_empty(), "a cfg'd item is exempt: {good:?}");
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
