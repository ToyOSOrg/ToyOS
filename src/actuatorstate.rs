//! A static that an actuator-guarded site *writes* is actuator state, and every
//! touch of it stands under an `actuator::` guard or inside a
//! `#[cfg(...-actuators)]` item — a touch outside both runs with nothing armed,
//! on the shipping kernel included, where the accessor is `const false` and the
//! guarded arm folds away. Writes and not reads decide the coupling: shipping
//! state an actuator merely consults is written by ordinary code everywhere,
//! and a read-coupling rule calls every one of those a refusal. Read by nothing
//! but its own tests, so it is not compiled into the build.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

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
/// no shipping kernel, so the property above is not theirs. `not(...)` forms
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

/// Every unguarded touch of actuator state, one line each. Pure over the
/// sources it is handed so the negative control stages files that are not on
/// disk.
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

    /// A flag stored under a guard in one file and loaded bare in another is
    /// refused; put under the accessor's condition, the same sources pass.
    #[test]
    fn a_guarded_store_with_a_bare_load_across_files_is_refused() {
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

    /// A second unconditional store of guarded state is a refusal; state an
    /// actuator only reads is free, and a `#[cfg(...-actuators)]` item exempt.
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
}
