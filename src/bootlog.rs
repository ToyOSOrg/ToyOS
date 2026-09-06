//! What a boot's own log says about that boot, and nothing else: the two
//! records that make a boot a pass, and the grammar for reading them.
//!
//! Neither the T14 driver's nor the QEMU harness's — `src/metal.rs` reads a log
//! off a flashed stick and `tests/common/power.rs` reads one off a guest's log
//! partition, and they must not be able to reach different answers about one
//! log. Pure: text in, a verdict out; its one gate reads the loader's source.

#![forbid(unsafe_code)]

use std::fmt;

/// The word the kernel writes as it hands the machine back to the firmware,
/// in `kernel/src/arch/syscall/machine.rs`'s `quiesce`.
pub const REBOOTING: &str = "Rebooting.";

/// The bootloader's own file on the log partition, which `logd` never writes
/// and no reader of `logd`'s files may pick up: it is not one of them, and the
/// verdict above is about what the kernel wrote.
pub const LOADER_LOG: &str = "loader.log";

/// That file's first line and its last, which are also the loader's first line
/// and the last it can write: the log is opened before the first, and
/// `ExitBootServices` takes the firmware's filesystem away after the last.
pub const LOADER_FIRST_LINE: &str = "ToyOS Bootloader 1.0";
pub const LOADER_LAST_LINE: &str = "Loader log: ExitBootServices, so this file ends here";

/// Whether `name` on the log volume is one of `logd`'s files. Every reader of
/// that volume asks this rather than the suffix: the loader's own file ends in
/// `.log` too, and it sorts after every dated stem.
pub fn is_logd_file(name: &str) -> bool {
    name.ends_with(".log") && name != LOADER_LOG
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

    /// Nothing links the two crates — the loader is `no_std` and this is the
    /// build system — so the three names above are held to the loader's by
    /// reading its source. The scan closes exactly one spelling of each: a
    /// quoted literal on one line. A name assembled from pieces reds here.
    #[test]
    fn the_loader_writes_the_file_the_host_reads() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("bootloader/src/loaderlog.rs");
        let source = std::fs::read_to_string(&path).expect("the loader's log module");
        assert!(
            source.contains(&format!("cstr16!(\"{LOADER_LOG}\")")),
            "{} does not open {LOADER_LOG:?}",
            path.display()
        );
        for line in [LOADER_FIRST_LINE, LOADER_LAST_LINE] {
            assert!(
                source.lines().any(|source_line| source_line.contains(&format!("\"{line}\""))),
                "{} does not declare {line:?}",
                path.display()
            );
        }
    }

    #[test]
    fn the_boot_record_is_the_kernels_own_line() {
        assert_eq!(boot_millis("[kernel 1.151 cpu0] Boot: complete (1151ms)\n"), Some(1151));
        assert_eq!(boot_millis("[kernel 0.084 cpu0] Boot: storage ready (84ms)\n"), None);
        assert_eq!(boot_millis("Boot: complete (later)\n"), None);
        assert_eq!(boot_millis(""), None);
    }
}
