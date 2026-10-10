//! Identifiers a tree may not name, and the exceptions that are named instead.

use std::path::{Path, PathBuf};

use crate::licence::COMMITTED_FILES;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The tree [`kernel_lines`] walks.
const KERNEL_SRC: &str = "kernel/src";

fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).unwrap_or(path).to_string_lossy().replace('\\', "/")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// `line` with its comment and its string literals removed.
fn code_only(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        match c {
            '\\' if in_string => {
                chars.next();
            }
            '"' => in_string = !in_string,
            '/' if !in_string && chars.peek() == Some(&'/') => break,
            _ if !in_string => out.push(c),
            _ => {}
        }
    }
    out
}

/// Whether the byte at `at` of `line` is a letter, a digit or `_`.
fn word_at(line: &str, at: Option<usize>) -> bool {
    at.and_then(|at| line.as_bytes().get(at)).is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
}

/// Whether `code` names `needle` as an identifier rather than as a fragment of
/// a longer one.
fn names(code: &str, needle: &str) -> bool {
    code.match_indices(needle)
        .any(|(at, _)| !word_at(code, at.checked_sub(1)) && !word_at(code, Some(at + needle.len())))
}

/// Every line of [`KERNEL_SRC`] under a relative path, with its number.
fn kernel_lines() -> Vec<(String, usize, String)> {
    let root = repo_root();
    let mut files = Vec::new();
    rust_files(&root.join(KERNEL_SRC), &mut files);
    let mut out = Vec::new();
    for path in files {
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        for (n, line) in text.lines().enumerate() {
            out.push((rel(&root, &path), n + 1, line.to_string()));
        }
    }
    out
}

/// The three log macros and the function they all expand to.
const LOG_PRODUCERS: &[&str] = &["log!(", "alert!(", "boot_phase!(", "log::emit("];

/// Every file whose whole contents run inside an NMI, and so may write no log
/// record: the handler, and the hard-lockup detector it samples through. The
/// detector's own control is not here — it stages from ordinary context, in
/// `kernel/src/hardlockup/probe.rs`, which is why it is a file of its own — and
/// neither is the line the arm writes, which its caller writes for it.
const NMI_SILENT: &[&str] =
    &["kernel/src/arch/x86_64/idt/nmi.rs", "kernel/src/hardlockup/mod.rs"];

/// Every `enable_bus_master(` site `kernel/src` holds, by file and count.
/// Arming DMA comes after a site's refusals — virtio parses its capability
/// chain first and disarms on one it cannot use — and the three early-enabling
/// MMIO drivers are `issues/bus-mastering-rides-memory-decode.md`'s to fix, not a precedent.
///
/// A function handed to a *process* takes the other path: `pcidev` arms bus
/// mastering with `start_bus_mastering`, after the address space that bounds it
/// exists, so `enable_bus_master` stays what a kernel driver calls and the
/// count below stays the census of drivers this kernel has.
const BUS_MASTER_SITES: &[(&str, usize)] = &[
    ("kernel/src/drivers/pci.rs", 1),
    ("kernel/src/drivers/virtio.rs", 1),
    ("kernel/src/drivers/hda.rs", 1),
    ("kernel/src/drivers/xhci/wait/boot.rs", 1),
];

/// The one place in `kernel/` a `!!!` may still appear, and how many times.
///
/// **Named by file and count rather than tested against the same line as a
/// `log!`.** A macro invocation is not a line — `rustfmt` puts a long one's
/// format string on its own — so "this line has `!!!` and a `log!` on it" is
/// defeated by a line break, and defeated silently. Every `!!!` in the tree is
/// listed here instead, so a new one is a red wherever it is written and
/// whatever it is written next to.
///
/// It writes raw bytes straight to the UART. They never enter the ring, so
/// `panic_console`'s deleted scan could not see them either and the record's
/// typed `Level` was never their business. Counted in occurrences of `!!!` and
/// not in lines, because it writes one at each end of its message.
///
/// **It was two.** `arch::idt::exceptions::debug_handler` put
/// `\n!!! DB TRAP !!!\n` out the port before it disarmed `DR7`, and the handler
/// went when `#DB` from Ring 3 became the ordinary Ring 3 fault it is. The gate
/// reds on a stale exemption as well as on a new marker, which is what made this
/// row part of that deletion rather than something to notice later.
const SENTINEL_ALLOWED: &[(&str, usize)] = &[
    // The two ends of `panic::last_words`' first line — `\n!!! <the dead end
    // this is> !!!` — written with the IDT possibly gone. Two whichever dead
    // end called it, because there is one writer of them.
    ("kernel/src/panic.rs", 2),
];

/// Every hand-written `Send`/`Sync` impl `kernel/src` holds, by file and count.
///
/// One of these stops the compiler re-deriving the bound, so a field added
/// later that is not `Send` — a raw pointer, an `Rc`, a `Cell` — keeps
/// compiling with nobody asked. Per file *and* per count, so an added impl
/// reds beside a permitted one and a deleted one reds its own stale row.
const AUTO_TRAIT_IMPLS: &[(&str, usize)] = &[
    ("kernel/src/drivers/hda.rs", 1),
    ("kernel/src/drivers/panic_console/mod.rs", 3),
    ("kernel/src/drivers/virtio_console.rs", 1),
    ("kernel/src/drivers/virtio_sound.rs", 2),
    ("kernel/src/arch/x86_64/hw.rs", 1),
    ("kernel/src/mm/mmio.rs", 2),
    ("kernel/src/mm/region.rs", 2),
    ("kernel/src/pipe.rs", 1),
    ("kernel/src/process.rs", 1),
    ("kernel/src/sched/driver.rs", 2),
    ("kernel/src/symbols.rs", 2),
];

/// Directories whose Rust is compiled for the guest, by repository-relative
/// prefix. **A crate's build script is host code and is walked**, whatever
/// prefix it is under. Both spellings --- the `build.rs` default and
/// whatever a `build = "…"` key names --- because the key overrides the name.
const GUEST_CODE: &[&str] = &[
    "rust",
    "kernel/src",
    "toyos/src",
    "toyos-abi/src",
    "sdk/std",
    "userland",
    "tests/toyos-rust-tests",
    "tests/testcases",
];

/// The host files that may name `temp_dir()` in code, and how many times.
/// Everything else takes its scratch from `toyos_tmpdir::TempDir`, which is
/// gone with its holder on a panic too.
const TEMP_DIR_ALLOWED: &[(&str, usize)] = &[
    // The guard itself.
    ("toyos-tmpdir/src/lib.rs", 1),
    // A failed golden comparison leaves its dump for the reader to copy over
    // the golden file: outliving the test is its purpose.
    ("toyos-keymap/tests/tables.rs", 1),
];

/// `bytes` look like a binary file by git's own heuristic: a NUL in the first
/// 8000 bytes.
///
/// A heuristic and not a proof — 8 KB of ASCII in front of a payload reads as
/// text — which is why `assets/` is walked whole rather than through this.
#[cfg(test)]
fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|b| *b == 0)
}

/// The compiler fork, which is upstream's tree.
#[cfg(test)]
const NOT_OURS: &str = "rust";

/// Every path a `build = "…"` key names, repository-relative.
///
/// Cargo's default is `build.rs` and the key overrides it, so a build script
/// under another name runs on this host and is reached by no walk keyed on the
/// filename. `userland/doom/Cargo.toml` sets the key today.
#[cfg(test)]
fn declared_build_scripts() -> std::collections::BTreeSet<String> {
    fn walk(root: &Path, dir: &Path, out: &mut std::collections::BTreeSet<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<_> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
        entries.sort();
        for path in entries {
            let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
            if name.starts_with('.') || name == "target" || rel(root, &path) == NOT_OURS {
                continue;
            }
            if path.is_dir() {
                walk(root, &path, out);
                continue;
            }
            if name != "Cargo.toml" {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let Ok(doc) = text.parse::<toml::Value>() else { continue };
            let Some(script) =
                doc.get("package").and_then(|p| p.get("build")).and_then(|b| b.as_str())
            else {
                continue;
            };
            let Some(dir) = path.parent() else { continue };
            out.insert(rel(root, &dir.join(script)));
        }
    }
    let root = repo_root();
    let mut out = std::collections::BTreeSet::new();
    walk(&root, &root, &mut out);
    out
}

/// Every `.rs` file under the repository that runs on the host: every one
/// outside [`GUEST_CODE`], plus each crate's build script inside it.
#[cfg(test)]
fn host_files() -> Vec<PathBuf> {
    fn walk(
        root: &Path,
        dir: &Path,
        scripts: &std::collections::BTreeSet<String>,
        out: &mut Vec<PathBuf>,
    ) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<_> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
        entries.sort();
        for path in entries {
            let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
            if name.starts_with('.') || name == "target" {
                continue;
            }
            let at = rel(root, &path);
            if at == NOT_OURS {
                continue;
            }
            if path.is_dir() {
                walk(root, &path, scripts, out);
                continue;
            }
            if !path.extension().is_some_and(|e| e == "rs") {
                continue;
            }
            let guest = GUEST_CODE
                .iter()
                .any(|skip| at == *skip || at.starts_with(&format!("{skip}/")));
            if !guest || name == "build.rs" || scripts.contains(&at) {
                out.push(path);
            }
        }
    }
    let root = repo_root();
    let scripts = declared_build_scripts();
    let mut out = Vec::new();
    walk(&root, &root, &scripts, &mut out);
    out
}

/// `bytes` as lower-case hex SHA-256, the spelling `NOTICE` records.
#[cfg(test)]
fn digest(bytes: &[u8]) -> String {
    toyos_sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// The shapes of a value that identifies a machine or the network it is on,
/// each by the name a finding gives it.
const MAC: &str = "a device's MAC address";
const PUBLIC_V4: &str = "a public IPv4 address";
const SHARED_V4: &str = "a 100.64/10 address";
const GLOBAL_V6: &str = "a global IPv6 address";
const LINK_LOCAL_V6: &str = "an interface's link-local IPv6 address";
const SERIAL: &str = "a device serial number";
const HOSTNAME: &str = "a hostname carrying a personal name";

/// Every value of a refused shape the tree keeps, by file, each identifying
/// nobody. A value a shape passes says the same without a row here: a locally
/// administered MAC, an RFC 5737 or RFC 3849 address.
const IDENTIFIES_NOBODY: &[(&str, &str)] = &[
    // Made up, in the kernel's own record of a disk that came back.
    ("tests/checks/usb.rs", "FEDCBA98765432FEDCBA"),
];

/// Every maximal run of `line`'s bytes that `of` takes, and where it starts.
/// `of` takes ASCII alone, so a run begins and ends on a character.
fn runs(line: &str, of: impl Fn(u8) -> bool) -> Vec<(usize, &str)> {
    let bytes = line.as_bytes();
    let (mut out, mut at) = (Vec::new(), 0);
    while at < bytes.len() {
        let start = at;
        while at < bytes.len() && of(bytes[at]) {
            at += 1;
        }
        if at > start {
            out.push((start, &line[start..at]));
        }
        at += usize::from(at == start);
    }
    out
}

/// Every run of hex digits and colons in `line`, less what a word owns of it.
/// A run that begins or ends inside a word shares that end with the word, up
/// to the colon nearest it: `MAC:` gives the address after it an octet's worth
/// of letters, and `IPv6:` a digit.
fn colon_runs(line: &str) -> Vec<&str> {
    runs(line, |b| b.is_ascii_hexdigit() || b == b':')
        .into_iter()
        .filter_map(|(start, run)| {
            let mut from = start;
            if word_at(line, start.checked_sub(1)) {
                from += run.find(':').map_or(run.len(), |at| at + 1);
            }
            let mut to = start + run.len();
            if word_at(line, Some(to)) {
                to = start + run.rfind(':').unwrap_or(0);
            }
            line.get(from..to)
        })
        .collect()
}

/// The MAC addresses `line` spells that a vendor gave a device: six octets and
/// no more, globally administered, unicast, and not the zero vendor a
/// placeholder is written with.
fn macs(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for run in colon_runs(line) {
        let spelled = run.trim_matches(':');
        // One digit or two: macOS's `arp` drops an octet's leading zero.
        let octets: Option<Vec<u8>> = spelled
            .split(':')
            .map(|octet| (octet.len() <= 2).then(|| u8::from_str_radix(octet, 16).ok()).flatten())
            .collect();
        if octets.is_some_and(|o| o.len() == 6 && o[0] & 0b11 == 0 && o[..3] != [0, 0, 0]) {
            out.push(spelled);
        }
    }
    out
}

/// The words a dotted quad follows where it is a section number.
const NUMBERED: [&str; 2] = ["§", "section"];

/// The IPv4 addresses `line` spells that are routed to one machine or one
/// network: four decimal octets and no more, outside a word, none of them a
/// section number, and outside the ranges that are everybody's.
fn v4s(line: &str) -> Vec<(&'static str, &str)> {
    let mut out = Vec::new();
    for (start, run) in runs(line, |b| b.is_ascii_digit() || b == b'.') {
        let spelled = run.trim_end_matches('.');
        let Ok(addr) = spelled.parse::<std::net::Ipv4Addr>() else { continue };
        if word_at(line, start.checked_sub(1)) || word_at(line, Some(start + run.len())) {
            continue;
        }
        let before = line[..start].trim_end().to_ascii_lowercase();
        if NUMBERED.iter().any(|word| before.ends_with(word)) {
            continue;
        }
        let [a, b, ..] = addr.octets();
        if a == 100 && (64..128).contains(&b) {
            out.push((SHARED_V4, spelled));
        } else if !(a == 0
            || a >= 224
            || addr.is_private()
            || addr.is_loopback()
            || addr.is_link_local()
            || addr.is_documentation())
        {
            out.push((PUBLIC_V4, spelled));
        }
    }
    out
}

/// The IPv6 addresses `line` spells that are one machine's: global unicast
/// outside RFC 3849's documentation prefix, or link-local with an interface
/// identifier longer than one group.
fn v6s(line: &str) -> Vec<(&'static str, &str)> {
    let mut out = Vec::new();
    for run in colon_runs(line) {
        // A sentence's colon after an address is not the address's.
        let spelled = if run.ends_with("::") { run } else { run.trim_end_matches(':') };
        let Ok(addr) = spelled.parse::<std::net::Ipv6Addr>() else { continue };
        let groups = addr.segments();
        if groups[0] & 0xe000 == 0x2000 && groups[..2] != [0x2001, 0x0db8] {
            out.push((GLOBAL_V6, spelled));
        } else if groups[0] & 0xffc0 == 0xfe80 && groups[4..7] != [0, 0, 0] {
            out.push((LINK_LOCAL_V6, spelled));
        }
    }
    out
}

/// What each `serial number` in `line` is followed by, where it is six or more
/// letters and digits with a digit among them: the kernel's record of a disk,
/// and the `Serial Number:` of a tool's report.
fn serials(line: &str) -> Vec<&str> {
    const NAMED: &str = "serial number";
    // What stands between those words and the serial, in a record, a report
    // or a string literal quoting either.
    const BETWEEN: [char; 7] = [' ', '\t', ':', '=', '"', '\'', '\\'];
    let lower = line.to_ascii_lowercase();
    let mut out = Vec::new();
    for (at, _) in lower.match_indices(NAMED) {
        let rest = line[at + NAMED.len()..].trim_start_matches(BETWEEN);
        let serial = &rest[..rest.bytes().take_while(u8::is_ascii_alphanumeric).count()];
        if serial.len() >= 6 && serial.bytes().any(|b| b.is_ascii_digit()) {
            out.push(serial);
        }
    }
    out
}

/// The names in `line` macOS gives a machine by default: its owner's first
/// name, an `s`, and the model.
fn personal_hostnames(line: &str) -> Vec<&str> {
    const MODELS: [&str; 5] = ["s-macbook", "s-imac", "s-mac-mini", "s-mac-studio", "s-mac-pro"];
    let lower = line.to_ascii_lowercase();
    let mut out = Vec::new();
    for model in MODELS {
        for (at, _) in lower.match_indices(model) {
            let name = lower[..at].bytes().rev().take_while(u8::is_ascii_alphabetic).count();
            if name > 0 {
                out.push(&line[at - name..at + model.len()]);
            }
        }
    }
    out
}

/// Every value in `line` of a shape that identifies a machine or its network.
fn identifying(line: &str) -> Vec<(&'static str, &str)> {
    let mut out: Vec<_> = macs(line).into_iter().map(|mac| (MAC, mac)).collect();
    out.extend(v4s(line));
    out.extend(v6s(line));
    out.extend(serials(line).into_iter().map(|serial| (SERIAL, serial)));
    out.extend(personal_hostnames(line).into_iter().map(|name| (HOSTNAME, name)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Two clauses, one rule each, and neither is checkable any other way.**
    ///
    /// The first: the NMI handler must not log. It would reenter its own CPU's
    /// log shard — the reservation is sound only because the CPU that owns the
    /// shard has `IF` and `TF` masked through publication, and an NMI is the one
    /// interrupt that ignores `IF`. This is what keeps it silent.
    ///
    /// The second: no log producer in `kernel/` carries `!!!` in its format
    /// string. `panic_console::has_alert` used to scan every display row for
    /// three exclamation marks, and its own comment enumerated the messages that
    /// happened to match; the panel reads `Level` off the record now, so a `!!!`
    /// put back into a message would be a marker marking nothing — a second,
    /// silent alert channel beside the typed one. The two `!!!` still in
    /// `kernel/` write raw bytes straight to the UART, never enter the ring, and
    /// were never `has_alert`'s business either.
    #[test]
    fn nmi_does_not_log() {
        let lines = kernel_lines();
        for file in NMI_SILENT {
            assert!(
                lines.iter().any(|(at, _, _)| at == file),
                "{file} moved: this gate is scanning a file that is not there"
            );
        }

        let silent: Vec<_> = lines
            .iter()
            .filter(|(file, _, line)| {
                NMI_SILENT.contains(&file.as_str())
                    && LOG_PRODUCERS.iter().any(|p| code_only(line).contains(p))
            })
            .map(|(file, n, line)| format!("{file}:{n}: {}", line.trim()))
            .collect();
        assert!(
            silent.is_empty(),
            "an NMI-reached file logs, and it reenters its own CPU's shard to do it:\n{}",
            silent.join("\n")
        );

        let mut found: Vec<(String, usize)> = Vec::new();
        for (file, _, line) in &lines {
            let n = line.matches("!!!").count();
            if n == 0 {
                continue;
            }
            match found.last_mut() {
                Some((last, count)) if last == file => *count += n,
                _ => found.push((file.clone(), n)),
            }
        }
        let mut complaints = Vec::new();
        for (file, count) in &found {
            match SENTINEL_ALLOWED.iter().find(|(allowed, _)| allowed == file) {
                Some((_, want)) if want == count => {}
                Some((_, want)) => complaints.push(format!(
                    "{file} has {count} `!!!` where this gate exempts {want} raw-UART ones"
                )),
                None => complaints.push(format!("{file} has {count} `!!!`")),
            }
        }
        for (file, want) in SENTINEL_ALLOWED {
            if !found.iter().any(|(f, _)| f == file) {
                complaints.push(format!(
                    "{file} no longer has the {want} raw-UART `!!!` this gate exempts, so the \
                     exemption is stale"
                ));
            }
        }
        assert!(
            complaints.is_empty(),
            "the `!!!` sentinel is deleted: the panel paints a red row from `Level::Alert` and \
             reads nothing out of the text, so a marker put back into a message marks \
             nothing.\n{}",
            complaints.join("\n")
        );
    }

    /// **A `Send`/`Sync` the compiler can derive is the compiler's to derive.**
    /// A hand-written one is a standing exemption from that re-derivation, so
    /// every one the kernel keeps is named here with the count its file holds.
    #[test]
    fn every_hand_written_auto_trait_impl_is_declared() {
        let mut found: Vec<(String, usize)> = Vec::new();
        for (file, _, line) in kernel_lines() {
            let code = code_only(&line);
            let code = code.trim_start();
            if !code.starts_with("unsafe impl Send for")
                && !code.starts_with("unsafe impl Sync for")
            {
                continue;
            }
            match found.last_mut() {
                Some((last, count)) if *last == file => *count += 1,
                _ => found.push((file, 1)),
            }
        }
        assert!(
            !found.is_empty(),
            "the scan found no hand-written impl at all, so it is reading no tree"
        );

        let mut complaints = Vec::new();
        for (file, count) in &found {
            match AUTO_TRAIT_IMPLS.iter().find(|(f, _)| f == file) {
                Some((_, want)) if want == count => {}
                Some((_, want)) => complaints.push(format!(
                    "{file} hand-writes {count} `Send`/`Sync` impls where this table declares \
                     {want}"
                )),
                None => complaints.push(format!(
                    "{file} hand-writes {count} `Send`/`Sync` impls and is not in this table"
                )),
            }
        }
        for (file, want) in AUTO_TRAIT_IMPLS {
            if !found.iter().any(|(f, _)| f == file) {
                complaints.push(format!(
                    "{file} no longer hand-writes the {want} impls declared here, so the row is \
                     stale"
                ));
            }
        }
        assert!(
            complaints.is_empty(),
            "a hand-written `Send`/`Sync` is a bound the compiler stops checking on every later \
             field, so each one is a row somebody wrote on purpose.\n{}",
            complaints.join("\n"),
        );
    }

    /// One process closing a capability cancels no other process's log poll:
    /// every `SysCap` names the log's one machine-wide watch, which outlives
    /// every handle, so `close_ends_polls` answers a `SysCap` with `false` and
    /// with nothing that could be anything else.
    #[test]
    fn a_capability_closing_ends_no_log_poll() {
        const OPS: &str = "kernel/src/object/ops.rs";
        const ARM: &str = "KObjectRef::SysCap(_) =>";
        let lines = kernel_lines();
        let body: Vec<String> = lines
            .iter()
            .filter(|(file, _, _)| file == OPS)
            .map(|(_, _, line)| code_only(line))
            .skip_while(|code| !code.contains("fn close_ends_polls("))
            .take_while(|code| code != "}")
            .collect();
        assert!(!body.is_empty(), "{OPS} has no `fn close_ends_polls(`: this gate reads nothing");
        let arms: Vec<&str> =
            body.iter().map(|code| code.trim()).filter(|code| code.starts_with(ARM)).collect();
        assert_eq!(
            arms,
            [format!("{ARM} false,")],
            "`close_ends_polls` must answer a `SysCap` with `false`: any other answer lets one \
             process's close cancel every log poll in the machine"
        );
    }

    /// Both directions: a resurrected early enable reds, and so does a stale row.
    #[test]
    fn bus_mastering_is_armed_at_exactly_the_declared_sites() {
        let mut counts = std::collections::BTreeMap::<String, usize>::new();
        for (file, _, line) in kernel_lines() {
            *counts.entry(file).or_default() += line.matches("enable_bus_master(").count();
        }
        let found: Vec<(String, usize)> = counts.into_iter().filter(|(_, n)| *n > 0).collect();
        for (file, n) in &found {
            let allowed = BUS_MASTER_SITES.iter().find(|(f, _)| f == file).map_or(0, |(_, c)| *c);
            assert_eq!(
                *n, allowed,
                "{file}: {n} × `enable_bus_master(`, {allowed} declared — arming DMA is a declared decision, and it comes after the site's refusals"
            );
        }
        for (file, n) in BUS_MASTER_SITES {
            let count = found.iter().find(|(f, _)| f == *file).map_or(0, |(_, c)| *c);
            assert_eq!(
                count, *n,
                "{file} is declared {n} × `enable_bus_master(` and has {count} — a stale row is a permission nobody re-argued"
            );
        }
    }

    /// The scan has teeth only over the files it opens, and "at least one" is
    /// a floor a walk of a single file also meets. The floor is the tree's own
    /// file list: every `.rs` file `git` tracks under it was read. An untracked
    /// one only adds to the walk.
    #[test]
    fn the_scan_reaches_the_tree_it_claims_to() {
        let root = repo_root();
        let walked: std::collections::BTreeSet<String> =
            kernel_lines().into_iter().map(|(file, _, _)| file).collect();
        let tracked: std::collections::BTreeSet<String> =
            crate::sysroot::tracked_files(&root, &[KERNEL_SRC]).unwrap_or_else(|e| panic!("{e}")).into_iter().filter(|p| p.ends_with(".rs")).collect();
        assert!(
            tracked.len() > 1,
            "git tracks {} .rs file(s) under {KERNEL_SRC}, so this floor is not one",
            tracked.len()
        );
        let missed: Vec<&String> = tracked.difference(&walked).collect();
        assert!(
            missed.is_empty(),
            "the walk over {KERNEL_SRC} read {} of the {} .rs files git tracks there, and missed \
             {} of them, the first being {:?}",
            walked.len(),
            tracked.len(),
            missed.len(),
            missed.first(),
        );
    }

    /// A page a process can write while the kernel is inside it never becomes a
    /// slice, and never an exclusive reference. Two files, named rather than
    /// walked: both spellings are ordinary elsewhere.
    ///
    /// A *shared* reference is not banned: `Ring::header` soundly takes one over
    /// the same page, its whole subject being an `AtomicU32`.
    #[test]
    fn no_slice_or_exclusive_reference_is_built_over_a_mapped_page() {
        const OVER_A_MAPPING: &[&str] = &["toyos-abi/src/ring.rs", "kernel/src/user_ptr.rs"];
        const SLICES: &[&str] =
            &["from_raw_parts", "from_raw_parts_mut", "from_ptr_range", "from_ptr_range_mut"];
        const EXCLUSIVE: &str = "&mut *";
        let root = repo_root();
        let mut complaints = Vec::new();
        for file in OVER_A_MAPPING {
            let text = std::fs::read_to_string(root.join(file))
                .unwrap_or_else(|e| panic!("{file}: {e} — the scan is looking elsewhere"));
            for (n, line) in text.lines().enumerate() {
                let code = code_only(line);
                for spelling in SLICES {
                    if names(&code, spelling) {
                        complaints.push(format!("{file}:{}: `{spelling}`", n + 1));
                    }
                }
                if code.contains(EXCLUSIVE) {
                    complaints.push(format!("{file}:{}: `{EXCLUSIVE}`", n + 1));
                }
            }
        }
        assert!(
            complaints.is_empty(),
            "a slice or an exclusive reference over a page a process can write \
             claims an exclusivity the mapping does not give:\n{}",
            complaints.join("\n"),
        );
    }

    /// **One scratch helper.** A directory made from `temp_dir()` by hand is
    /// left behind by the panic that fails its test; `toyos_tmpdir::TempDir`
    /// is not. Each exception is counted exactly, so it goes when its call
    /// site does.
    #[test]
    fn every_host_scratch_is_the_guard() {
        let root = repo_root();
        let mut found = std::collections::BTreeMap::new();
        for path in host_files() {
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let n = text.lines().filter(|line| code_only(line).contains("temp_dir()")).count();
            if n > 0 {
                found.insert(rel(&root, &path), n);
            }
        }
        let allowed: std::collections::BTreeMap<String, usize> =
            TEMP_DIR_ALLOWED.iter().map(|(file, n)| (file.to_string(), *n)).collect();
        assert_eq!(
            found, allowed,
            "`temp_dir()` in host code outside the named exceptions: take a \
             `toyos_tmpdir::TempDir` instead"
        );
    }

    /// **Every committed file somebody had to judge is judged here.** `NOTICE`
    /// names each one's upstream, licence and digest; nothing read it, so a new
    /// one arrived unremarked and a changed one changed silently. The digest is
    /// taken from the bytes and held against the row and, for a third-party
    /// file, against `NOTICE` itself.
    #[test]
    fn every_committed_binary_file_is_declared() {
        let root = repo_root();
        let listing = crate::sysroot::tracked_files(&root, &[]).unwrap_or_else(|e| panic!("{e}"));

        let mut found: Vec<(String, String)> = Vec::new();
        for name in &listing {
            let Ok(bytes) = std::fs::read(root.join(name)) else { continue };
            if !is_binary(&bytes) && !name.starts_with("assets/") {
                continue;
            }
            found.push((name.to_string(), digest(&bytes)));
        }
        assert!(
            found.iter().any(|(name, _)| name == "assets/DOOM1.WAD"),
            "the walk found no DOOM1.WAD, so it is not reading the tracked tree"
        );
        assert!(
            found.iter().any(|(name, _)| name == "assets/icons/x-bold.svg"),
            "the walk found no Phosphor SVG, so `assets/` is not being read whole"
        );

        let notice = std::fs::read_to_string(root.join("NOTICE")).expect("NOTICE");
        let mut complaints = Vec::new();
        for (name, sha) in &found {
            match COMMITTED_FILES.iter().find(|(f, _, _, _)| f == name) {
                None => complaints
                    .push(format!("{name} is committed and nothing declares it")),
                Some((_, want, _, _)) if want != sha => {
                    complaints.push(format!("{name} is sha256 {sha} where it is declared {want}"))
                }
                Some((_, _, "NOTICE", _)) if !notice.contains(sha.as_str()) => complaints
                    .push(format!("{name} is sha256 {sha}, which NOTICE does not carry")),
                Some(_) => {}
            }
        }
        for (name, _, where_from, _) in COMMITTED_FILES {
            if !found.iter().any(|(f, _)| f == name) {
                complaints.push(format!(
                    "{name} is declared here ({where_from}) and is no longer committed"
                ));
            }
        }
        assert!(
            complaints.is_empty(),
            "a committed binary is a file somebody had to judge, and NOTICE is where the \
             judgement is:\n{}",
            complaints.join("\n"),
        );
    }

    /// **No tracked file carries a value that identifies a machine or its
    /// network.** This repository is public, and a captured log or command
    /// reply carries such a value whoever pastes it: every tracked file is read
    /// for the shapes, and each one found is named by file, line and kind and
    /// by no character of it, because a red gate's log is posted. A stale row
    /// reds too, which is also what says the scan read the tree.
    ///
    /// Only text has a shape. A value spelled as bytes, a GUID, and a serial
    /// that nothing on its line calls a serial number are the reader's to see.
    /// A home-directory path carrying the owner's first name is not private,
    /// and is no shape here.
    #[test]
    fn no_tracked_file_identifies_a_machine_or_its_network() {
        let root = repo_root();
        let listing = crate::sysroot::tracked_files(&root, &[]).unwrap_or_else(|e| panic!("{e}"));
        let mut complaints = Vec::new();
        let mut kept = std::collections::BTreeSet::new();
        // The fork's gitlink is a commit, and every other tracked path a file.
        for name in listing.iter().filter(|name| *name != NOT_OURS) {
            let bytes = std::fs::read(root.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
            for (n, line) in String::from_utf8_lossy(&bytes).lines().enumerate() {
                for (shape, value) in identifying(line) {
                    match IDENTIFIES_NOBODY.iter().find(|row| **row == (name.as_str(), value)) {
                        Some(row) => {
                            kept.insert(*row);
                        }
                        None => complaints.push(format!("{name}:{}: {shape}", n + 1)),
                    }
                }
            }
        }
        for row in IDENTIFIES_NOBODY.iter().filter(|row| !kept.contains(*row)) {
            complaints.push(format!("{} no longer carries {:?}, so its row is stale", row.0, row.1));
        }
        assert!(
            complaints.is_empty(),
            "a value that identifies a machine or its network is private, and this tree is not: \
             write one that identifies nobody (a locally administered MAC, an RFC 5737 or RFC \
             3849 address, a made-up serial or name). A section number that reads as an address \
             takes a `§` in front of it.\n{}",
            complaints.join("\n"),
        );
    }

    /// Each shape is refused and what resembles it is not. The refused values
    /// are assembled here, so that this file spells none.
    #[test]
    fn a_value_is_refused_by_its_shape_and_what_resembles_one_is_not() {
        let shapes = |line: &str| identifying(line).into_iter().map(|(shape, _)| shape).collect::<Vec<_>>();
        let mac = ["00", "11", "22", "33", "44", "55"].join(":");
        let public = [203, 0, 114, 7].map(|octet: u8| octet.to_string()).join(".");
        let shared = [100, 64, 0, 1].map(|octet: u8| octet.to_string()).join(".");
        let global = ["2a00", "1", "", "1"].join(":");
        let prefix = ["2a00", "1", "2", "300", "", ""].join(":");
        let link_local = ["fe80", "", "1c2d", "3e4f", "5a6b", "7c8d"].join(":");
        for (line, shape) in [
            (format!("netstack: MAC {mac}"), MAC),
            (format!("MAC:{mac}: the lease went to it"), MAC),
            (format!("{mac}:eth0 took the lease"), MAC),
            (format!("? (10.0.2.2) at {} on en0", ["0", "11", "22", "3", "44", "55"].join(":")), MAC),
            (format!("dns [{public}]"), PUBLIC_V4),
            (format!("see §4.1 for {public}."), PUBLIC_V4),
            (format!("tailscale0 UNKNOWN {shared}/32"), SHARED_V4),
            (format!("inet6 {global}/64"), GLOBAL_V6),
            (format!("addr:{global}"), GLOBAL_V6),
            (format!("delegated {prefix}/56"), GLOBAL_V6),
            (format!("IPv6:{link_local}"), LINK_LOCAL_V6),
            (format!("inet6 {link_local}: link"), LINK_LOCAL_V6),
            (format!("USB 0781:5581, serial number \\\"{}\\\"", ["A1B2", "C3D4"].concat()), SERIAL),
            (format!("Serial Number: {}", ["PF", "000000"].concat()), SERIAL),
            (["Somebody", "s-MacBook-Air.local"].concat(), HOSTNAME),
            (["somebody", "s-imac"].concat(), HOSTNAME),
        ] {
            assert_eq!(shapes(&line), [shape], "{line}");
        }
        for line in [
            "netstack: MAC 52:54:00:12:34:56, ff:ff:ff:ff:ff:ff, 01:00:5e:00:00:fb, 33:33:00:00:00:01",
            "wire_mac 02:00:00:aa:bb:cc and the placeholder 00:00:00:00:00:01",
            "a longer run of octets 0a:1b:2c:3d:4e:5f:60:71, and 16:08:23 on 2026-09-08",
            "leased 10.0.2.15/24 from 10.0.2.2, 127.0.0.1, 169.254.1.1, 192.168.1.46, 172.16.0.1",
            "192.0.2.4 198.51.100.7 203.0.113.9 224.0.0.251 0.0.0.0 240.0.0.0/4 255.255.255.255.",
            "PCIe base spec 6.0 §7.5.3.3, §7.5.3.4 and §7.5.3.16, v1.2.3.4, 1.2.3.4.5, D7.5.9.2",
            "Architecture Specification §3.2.5.6 to §3.2.5.8, Section 6.5.2.7, 612523 §9.5.9.2.23",
            "fe80::1 2001:db8::1 ff02::fb ::1 fd00::5, and `c::{name}` in std::net::Ipv6Addr",
            "usb-storage: slot {slot_id} serial number {}, and its serial number differs",
            "Lab-MacBook-Air.local, toyos-t14.local, s-macbook",
        ] {
            let found = shapes(line);
            assert!(found.is_empty(), "{line}: {found:?}");
        }
    }
}
