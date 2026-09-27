//! The disabled list: the tests that do not run, each with the issue that owns
//! its defect.
//!
//! A red is a red, so a failing test, flaky or not, is fixed or disabled here
//! with its issue. A disabled test runs nowhere: `tests/toyos.rs` skips it and
//! names it and its issue on every run, and refuses a row whose test nothing
//! registers or whose issue file does not exist. The change that fixes the test
//! deletes its row.
//!
//! `cargo run -- --known-red <test>` answers whether a test is disabled; with
//! no argument it prints the list.

use std::path::Path;

use crate::flags;

/// One disabled test.
#[derive(PartialEq, Eq, Debug)]
pub struct Disabled {
    /// The registered test name, exactly.
    pub test: &'static str,
    /// The issue file that owns the defect.
    pub issue: &'static str,
}

/// Every disabled test.
pub const DISABLED: &[Disabled] = &[
    Disabled { test: "console_line_atomicity", issue: "issues/build/parallel-tests-red-under-other-suites.md" },
    Disabled {
        test: "console_locale_detect",
        issue: "issues/build/the-console-input-path-can-stop-after-a-ps2-overflow.md",
    },
    Disabled { test: "desktop_window_child", issue: "issues/kernel/desktop-window-child-freeze.md" },
    Disabled { test: "doom_sound_flood", issue: "issues/audio/doom-sound-flood-played-full-scale-once.md" },
    Disabled { test: "handle_transfer", issue: "issues/kernel/deferred-release-outlives-its-syscall.md" },
    Disabled { test: "hda_tone", issue: "issues/audio/hda-tone-phase-check.md" },
    Disabled { test: "kill_while_blocked", issue: "issues/kernel/deferred-release-outlives-its-syscall.md" },
    Disabled { test: "latency_wake", issue: "issues/build/latency-wake-reds-on-the-dev-host-at-a-rate.md" },
    Disabled {
        test: "sched_check_build",
        issue: "issues/build/the-pass-cost-gates-ci-sample-is-eight-days-stale-twice.md",
    },
    Disabled {
        test: "screen_fatal_halt",
        issue: "issues/boot-media/screen-fatal-halt-reds-on-ci-with-a-usb-storage-transport-break-during-boot.md",
    },
    Disabled {
        test: "short_sleep_livelock",
        issue: "issues/kernel/short-sleep-livelock-stalls-on-ci-with-one-sleeper-never-returning.md",
    },
    Disabled {
        test: "so_cache_refusals",
        issue: "issues/kernel/so-cache-refusals-saw-the-kernel-refuse-nothing-once.md",
    },
    Disabled { test: "usb_disk_index_stable", issue: "issues/hardware/eleven-names-red-on-ci.md" },
];

/// The row that disables `test`, matched by the whole name.
pub fn disabled(test: &str) -> Option<&'static Disabled> {
    DISABLED.iter().find(|row| row.test == test)
}

/// Every row of `rows` against the tree under `root`: no test twice, each one
/// `registered`, and each issue a file under `issues/`.
pub fn check(rows: &[Disabled], registered: impl Fn(&str) -> bool, root: &Path) -> Result<(), String> {
    for (at, row) in rows.iter().enumerate() {
        if rows[..at].iter().any(|earlier| earlier.test == row.test) {
            return Err(format!("{} is disabled twice", row.test));
        }
        if !registered(row.test) {
            return Err(format!(
                "{} is disabled and nothing registers it: a renamed or deleted test takes its row \
                 with it",
                row.test
            ));
        }
        if !row.issue.starts_with("issues/") || !root.join(row.issue).is_file() {
            return Err(format!("{}: `{}` is not an issue file in this tree", row.test, row.issue));
        }
    }
    Ok(())
}

/// `cargo run -- --known-red [<test>]`.
pub fn dispatch(args: &[String]) {
    print!("{}", answer(DISABLED, flags::CARGO_RUN.value(args, &flags::KNOWN_RED)));
}

fn answer(rows: &[Disabled], asked: Option<&str>) -> String {
    let Some(test) = asked else {
        return rows.iter().map(|row| format!("{}  {}\n", row.test, row.issue)).collect();
    };
    match rows.iter().find(|row| row.test == test) {
        Some(row) => format!("{test}: YES, disabled — it does not run.\n  {}\n", row.issue),
        None => format!("{test}: NO, not disabled — it runs, and its red fails the suite.\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR"))
    }

    const ISSUE: &str = "issues/README.md";

    #[test]
    fn every_row_names_one_test_and_an_issue_file_that_exists() {
        check(DISABLED, |_| true, root()).unwrap();
    }

    #[test]
    fn a_row_is_refused_for_an_unregistered_test_a_missing_issue_or_a_second_row() {
        let registered = |name: &str| name == "a_real_test";
        check(&[Disabled { test: "a_real_test", issue: ISSUE }], registered, root()).unwrap();
        check(&[], registered, root()).unwrap();
        let refused = |rows: &[Disabled], why: &str| {
            let said = check(rows, registered, root()).unwrap_err();
            assert!(said.contains(why), "{said}");
        };
        refused(&[Disabled { test: "a_renamed_test", issue: ISSUE }], "nothing registers it");
        refused(&[Disabled { test: "a_real_test", issue: "issues/no-such-issue.md" }], "not an issue file");
        refused(&[Disabled { test: "a_real_test", issue: "CLAUDE.md" }], "not an issue file");
        refused(
            &[Disabled { test: "a_real_test", issue: ISSUE }, Disabled { test: "a_real_test", issue: ISSUE }],
            "disabled twice",
        );
    }

    #[test]
    fn a_row_disables_its_whole_name_and_nothing_that_extends_it() {
        for row in DISABLED {
            assert_eq!(disabled(row.test), Some(row));
            assert_eq!(disabled(&format!("{}_controls", row.test)), None);
        }
        assert_eq!(disabled(""), None);
    }

    #[test]
    fn the_answer_is_yes_or_no_with_the_issue() {
        let rows = [Disabled { test: "reds", issue: "issues/x.md" }];
        let yes = answer(&rows, Some("reds"));
        assert!(yes.starts_with("reds: YES") && yes.contains("issues/x.md"), "{yes}");
        assert!(answer(&rows, Some("greens")).starts_with("greens: NO"));
        assert_eq!(answer(&rows, None), "reds  issues/x.md\n");
    }
}
