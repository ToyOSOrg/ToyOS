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

/// The names the global registry left behind, and one that is not a name at
/// all: `services::connect` was the call that resolved one.
///
/// Each is retired rather than renamed — `SYS_CONNECTION_JOIN` keeps number 76
/// and is a different call, addressed by handle, granting nothing. A word
/// boundary is what tells the two apart here.
const RETIRED_REGISTRY: &[&str] = &[
    "SYS_CONNECT",
    "SYS_LISTEN",
    "SYS_PIPE_OPEN",
    "SYS_PIPE_ID",
    "SYS_SOCKET_CREATE",
    "SharedToken",
    "services::connect",
];

/// Every other ABI name this project has retired: a deleted syscall, debug
/// action or inbox op code, whose *number* is retired with it and never
/// reused (`CLAUDE.md`, "Syscall ABI").
///
/// The number is what the rule protects and a number cannot be scanned for —
/// so the name is, and a name back in code is how a number gets reissued by
/// accident. Retired numbers themselves are recorded where they can be read
/// beside the live ones: the comments in `toyos-abi/src/syscall.rs` and
/// `toyos-abi/src/inbox.rs`, which this scan is blind to by construction
/// because it strips comments.
///
/// **A rename is not a retirement, and this table gained no row for one.**
/// `SYS_IO_URING_SETUP`/`SYS_IO_URING_ENTER` became `SYS_INBOX_SETUP`/
/// `SYS_INBOX_SUBMIT` on 2026-08-20 keeping numbers 89 and 90, the same
/// arguments and the same struct layouts, so nothing was deleted and no number
/// is protectable by forbidding the old spelling.
const RETIRED_ABI_NAMES: &[&str] = &[
    // Syscall 107. Nothing called it; a region's mappings go with its last
    // handle, so the handle is the whole of letting go.
    "SYS_SHM_UNMAP",
    // `SYS_DEBUG` actions 14 and 15. A total hides a leak of one kind behind
    // churn in another, and a breakdown in the kernel log is a reading no guest
    // test can see; every leak assertion in the estate is `CENSUS_KIND`.
    "CENSUS_TOTAL",
    "CENSUS_BREAKDOWN",
    // Inbox op code 2. No submitter anywhere: this kernel's watches are
    // one-shot and mio re-arms rather than cancels. Retired under both the
    // spelling it carried when it was deleted and the one a reintroduction
    // would write in today's vocabulary.
    "IORING_OP_POLL_REMOVE",
    "OP_POLL_REMOVE",
    "OP_CANCEL",
    // Inbox op code 4, `IORING_OP_CLOSE`: the one handle path that could not
    // obey the bad-handle policy, running under the ring's own lock. Same two
    // vocabularies.
    "IORING_OP_CLOSE",
    "OP_CLOSE",
    // Syscall 8. The monotonic clock is a page every address space maps
    // read-only (`toyos_abi::clock`), so reading it is no transition at all.
    "SYS_CLOCK",
];

/// Everything this repository compiles into the guest.
const GUEST_TREES: &[&str] =
    &["kernel/src", "toyos/src", "toyos-abi/src", "userland", "tests"];

/// `line` with its comment and its string literals removed.
///
/// What is left is the part that names things. Prose explaining what a deleted
/// call used to do is legal and worth keeping; a gravestone table mapping a
/// retired number to the string `"SYS_LISTEN"` is the point of the table.
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

/// Whether `code` names `needle` as an identifier rather than as a fragment of
/// a longer one.
fn names(code: &str, needle: &str) -> bool {
    let bytes = code.as_bytes();
    let word = |b: Option<&u8>| b.is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_');
    code.match_indices(needle).any(|(at, _)| {
        !word(at.checked_sub(1).and_then(|j| bytes.get(j)))
            && !word(bytes.get(at + needle.len()))
    })
}

/// `(file, line number)` for every place `needle` is named in code, over
/// [`GUEST_TREES`].
fn named_in_code(needle: &str) -> Vec<String> {
    let root = repo_root();
    let mut files = Vec::new();
    for tree in GUEST_TREES {
        rust_files(&root.join(tree), &mut files);
    }
    let mut found = Vec::new();
    for path in files {
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        for (n, line) in text.lines().enumerate() {
            if names(&code_only(line), needle) {
                found.push(format!("{}:{}", rel(&root, &path), n + 1));
            }
        }
    }
    found
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
/// MMIO drivers are `issues/isolation/`'s to fix, not a precedent.
///
/// A function handed to a *process* takes the other path: `pcidev` arms bus
/// mastering with `start_bus_mastering`, after the address space that bounds it
/// exists, so `enable_bus_master` stays what a kernel driver calls and the
/// count below stays the census of drivers this kernel has.
const BUS_MASTER_SITES: &[(&str, usize)] = &[
    ("kernel/src/drivers/pci.rs", 1),
    ("kernel/src/drivers/virtio.rs", 1),
    ("kernel/src/drivers/hda.rs", 1),
    ("kernel/src/drivers/nvme.rs", 1),
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
    ("kernel/src/trace.rs", 1),
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
    "userland",
    "tests/toyos-rust-tests",
    "tests/iced-counter",
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

/// The compiler fork, which is upstream's tree and is judged by
/// `src/forkcheck.rs` rather than by anything here.
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
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Two clauses, one rule each, and neither is checkable any other way.**
    ///
    /// The first: the NMI handler must not log. It would reenter its own CPU's
    /// log shard — the reservation is sound only because the CPU that owns the
    /// shard has `IF` and `TF` masked through publication, and an NMI is the one
    /// interrupt that ignores `IF`. `dump_nmi_probe` is what makes the handler
    /// *useful*; this is what keeps it silent.
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

    /// **There is no global registry.** A name a process could present and have
    /// resolved for it is the thing this architecture deletes, so its
    /// identifiers may not be reachable from any code the guest compiles.
    #[test]
    fn no_name_resolves_through_a_registry_any_more() {
        let mut complaints = Vec::new();
        for needle in RETIRED_REGISTRY {
            for at in named_in_code(needle) {
                complaints.push(format!("{at}: names `{needle}`"));
            }
        }
        assert!(
            complaints.is_empty(),
            "the registry is deleted, and these still name it:\n  {}",
            complaints.join("\n  "),
        );
    }

    /// **A retired ABI number is never reused**, and the name is the only part
    /// of it a scan can hold on to. A retired name back in guest-compiled code
    /// is either the number coming back or a new call wearing a dead one's
    /// identity, and the two are indistinguishable from the outside.
    #[test]
    fn a_retired_abi_name_is_gone_from_the_code() {
        let mut complaints = Vec::new();
        for needle in RETIRED_ABI_NAMES {
            for at in named_in_code(needle) {
                complaints.push(format!("{at}: names `{needle}`"));
            }
        }
        assert!(
            complaints.is_empty(),
            "these names are retired and their numbers with them:\n  {}",
            complaints.join("\n  "),
        );
    }

    /// What the scan above can and cannot see, stated as cases, because a
    /// well-formed tree exercises none of them.
    #[test]
    fn the_registry_scan_reads_code_and_not_prose() {
        assert!(names(&code_only("    let x = syscall(SYS_LISTEN, 0);"), "SYS_LISTEN"));
        assert!(names(&code_only("pub const SYS_PIPE_ID: u64 = 70;"), "SYS_PIPE_ID"));
        assert!(!names(&code_only("/// `SYS_LISTEN` used to register a name."), "SYS_LISTEN"));
        assert!(!names(&code_only("    // SYS_PIPE_ID was 70"), "SYS_PIPE_ID"));
        assert!(!names(&code_only("    85 => \"SYS_LISTEN\","), "SYS_LISTEN"));
        // The live call keeps the retired one's number and must not be read as
        // it: this is the whole reason the match is on a word boundary.
        assert!(!names(&code_only("SYS_CONNECTION_JOIN => join(a, b),"), "SYS_CONNECT"));
        assert!(names(&code_only("SYS_CONNECT => connect(a),"), "SYS_CONNECT"));
        // And the walk reaches real code: a live name it is capable of finding
        // must actually be found.
        assert!(
            !named_in_code("SYS_CONNECTION_JOIN").is_empty(),
            "the scan found no `SYS_CONNECTION_JOIN` in code, so it is not reading the guest trees",
        );
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
}
