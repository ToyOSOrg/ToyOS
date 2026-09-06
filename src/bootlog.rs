//! What a boot's own log says about that boot, and nothing else: the two
//! records that make a boot a pass, and the grammar for reading them.
//!
//! Neither the T14 driver's nor the QEMU harness's — `src/metal.rs` reads a log
//! off a flashed stick and `tests/common/power.rs` reads one off a guest's log
//! partition, and they must not be able to reach different answers about one
//! log. Text in, a verdict out; its one gate reads the loader's source.

#![forbid(unsafe_code)]

use std::fmt;

/// The word the kernel writes as it hands the machine back to the firmware,
/// in `kernel/src/arch/syscall/machine.rs`'s `quiesce`.
pub const REBOOTING: &str = "Rebooting.";

/// The bootloader's own file at the root of the log partition.
pub const LOADER_LOG: &str = "loader.log";

/// That file's first line and its last.
pub const LOADER_FIRST_LINE: &str = "ToyOS Bootloader 1.0";
pub const LOADER_LAST_LINE: &str = "Loader log: the kernel handoff begins, so this file ends here";

/// Whether `name` on the log volume is one of `logd`'s files, which is
/// `logd`'s own allow-list and not a suffix: the loader's file ends in `.log`
/// too, and a `toybox` run can leave anything there.
pub fn is_logd_file(name: &str) -> bool {
    toyos_wallclock::classify(name).is_some()
}

/// The names on a mounted log volume, split into the loader's file and
/// `logd`'s in the order theirs sort.
///
/// The loader's is matched without case, because a FAT driver that does not
/// read the lowercase flags in a directory entry yields `LOADER.LOG`; `logd`'s
/// are matched as its own writer spells them, which no such driver preserves
/// either — a volume read through one has no `logd` file this can name, and
/// says so by finding none.
pub fn split_listing(listing: &str) -> (Option<&str>, Vec<&str>) {
    let mut loader = None;
    let mut logd = Vec::new();
    for name in listing.lines().map(str::trim).filter(|name| !name.is_empty()) {
        if name.eq_ignore_ascii_case(LOADER_LOG) {
            loader = Some(name);
        } else if is_logd_file(name) {
            logd.push(name);
        }
    }
    logd.sort_unstable();
    (loader, logd)
}

/// The kernel's boot-phase record for the end of boot, in
/// `kernel/src/log/mod.rs`'s `boot_phase!`.
const COMPLETE: &str = "Boot: complete (";

/// Why a log is not a passing boot's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unfit {
    NoBootRecord,
    /// The log does not end at the reset: the last line it carries instead.
    Unfinished(String),
}

impl fmt::Display for Unfit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoBootRecord => write!(f, "the log carries no `{COMPLETE}Nms)` record"),
            Self::Unfinished(saw) => write!(
                f,
                "the log's last line is {saw:?} and not {REBOOTING:?}: either the boot never \
                 handed the machine back to the firmware, or the reset outran logd"
            ),
        }
    }
}

/// The boot's own duration, out of `Boot: complete (123ms)`.
pub fn boot_millis(log: &str) -> Option<u64> {
    let tail = log.lines().find_map(|line| line.split(COMPLETE).nth(1))?;
    tail.split("ms)").next()?.parse().ok()
}

/// A boot's duration if its log is a passing boot's, which takes both records:
/// a log ending anywhere but the reset is a machine that did not come back on
/// its own, so the word is looked for as the last line and not in the text.
pub fn verdict(log: &str) -> Result<u64, Unfit> {
    let boot_ms = boot_millis(log).ok_or(Unfit::NoBootRecord)?;
    let last = log.lines().rev().find(|line| !line.trim().is_empty()).unwrap_or_default();
    if !last.contains(REBOOTING) {
        return Err(Unfit::Unfinished(last.trim().to_string()));
    }
    Ok(boot_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The half-told boot: the kernel got all the way up and the log stops
    /// there, so the machine either never asked for the reset or the reset
    /// outran `logd`.
    #[test]
    fn a_boot_record_without_the_reset_word_is_not_a_pass() {
        let booted = "[kernel 1.151 cpu0] Boot: complete (1151ms)\n";
        let ended = format!("{booted}[logd 1.203 cpu1] {REBOOTING}\n");
        assert_eq!(verdict(&ended), Ok(1151));
        // Trailing blank lines are not the last line.
        assert_eq!(verdict(&format!("{ended}\n  \n")), Ok(1151));

        assert_eq!(
            verdict(booted),
            Err(Unfit::Unfinished("[kernel 1.151 cpu0] Boot: complete (1151ms)".to_string()))
        );
        let carried_on = format!("{ended}[kernel 1.400 cpu0] hda: codec 0 reset\n");
        assert!(matches!(verdict(&carried_on), Err(Unfit::Unfinished(_))));
        assert_eq!(verdict(&format!("[logd 0.9 cpu1] {REBOOTING}\n")), Err(Unfit::NoBootRecord));
        assert_eq!(verdict(""), Err(Unfit::NoBootRecord));
    }

    /// Whether `source` declares a constant whose value is exactly `rhs`.
    ///
    /// Anchored to the declaration, so a name that appears in a message or in
    /// a longer literal is not one: the line must end `= <rhs>;`.
    fn declares(source: &str, rhs: &str) -> bool {
        let tail = format!("= {rhs};");
        source.lines().any(|line| line.trim_end().ends_with(&tail))
    }

    #[test]
    fn only_a_declaration_of_the_whole_value_counts() {
        assert!(declares("const A: &str = \"x\";", "\"x\""));
        assert!(declares("    const A: &CStr16 = cstr16!(\"x\");   ", "cstr16!(\"x\")"));
        // A longer literal that carries the value, and a mention in a message.
        assert!(!declares("const A: &str = \"xy\";", "\"x\""));
        assert!(!declares("    say(\"x\");", "\"x\""));
        // The value under another spelling, and concatenated.
        assert!(!declares("const A: &CStr16 = cstr16!(\"x\");", "\"x\""));
        assert!(!declares("const A: &str = \"x\" \"y\";", "\"xy\""));
    }

    /// Nothing links the two crates: the loader is `no_std` and this is the
    /// build system, so the three names above are held to the loader's own
    /// declarations by reading its source.
    #[test]
    fn the_loader_writes_the_file_the_host_reads() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("bootloader/src/loaderlog.rs");
        let source = std::fs::read_to_string(&path).expect("the loader's log module");
        let wanted = [
            format!("cstr16!(\"{LOADER_LOG}\")"),
            format!("\"{LOADER_FIRST_LINE}\""),
            format!("\"{LOADER_LAST_LINE}\""),
        ];
        for rhs in wanted {
            assert!(
                declares(&source, &rhs),
                "{} declares no constant equal to {rhs}",
                path.display()
            );
        }
    }

    #[test]
    fn the_loaders_file_is_told_from_logds_however_a_driver_spelled_it() {
        let listing = "2026-09-06-084003.log\nloader.log\nunknown-00.log\nnotes.txt\n";
        assert_eq!(
            split_listing(listing),
            (Some("loader.log"), vec!["2026-09-06-084003.log", "unknown-00.log"])
        );
        // A FAT driver that drops the lowercase flags yields 8.3 in upper case.
        assert_eq!(split_listing("LOADER.LOG\n").0, Some("LOADER.LOG"));
        // And it is never one of logd's, under either spelling.
        assert!(split_listing("LOADER.LOG\nloader.log\n").1.is_empty());
        // Blank rows and stray whitespace are a listing's, not a name's.
        assert_eq!(split_listing("\n  loader.log  \n\n").0, Some("loader.log"));
        assert_eq!(split_listing(""), (None, Vec::new()));
        // Somebody else's file, which nothing here may name or delete.
        assert_eq!(split_listing("boot.log\n"), (None, Vec::new()));
    }

    #[test]
    fn the_boot_record_is_the_kernels_own_line() {
        assert_eq!(boot_millis("[kernel 1.151 cpu0] Boot: complete (1151ms)\n"), Some(1151));
        assert_eq!(boot_millis("[kernel 0.084 cpu0] Boot: storage ready (84ms)\n"), None);
        assert_eq!(boot_millis("Boot: complete (later)\n"), None);
        assert_eq!(boot_millis(""), None);
    }
}
