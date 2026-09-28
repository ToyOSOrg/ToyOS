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
    Disabled {
        test: "console_line_atomicity",
        issue: "issues/build/console-line-atomicity-loses-five-of-a-thousand-lines-on-ci.md",
    },
    Disabled {
        test: "console_locale_detect",
        issue: "issues/build/the-console-input-path-can-stop-after-a-ps2-overflow.md",
    },
    Disabled { test: "desktop_window_child", issue: "issues/kernel/desktop-window-child-freeze.md" },
    Disabled { test: "doom_sound_flood", issue: "issues/audio/doom-sound-flood-played-full-scale-once.md" },
    Disabled {
        test: "handle_kill_policy",
        issue: "issues/kernel/handle-kill-policy-census-grew-one-sharedmem-on-two-nightlies.md",
    },
    Disabled { test: "handle_transfer", issue: "issues/kernel/deferred-release-outlives-its-syscall.md" },
    Disabled { test: "hda_tone", issue: "issues/audio/hda-tone-phase-check.md" },
    Disabled {
        test: "i8042_mouse",
        issue: "issues/hardware/i8042-mouse-ends-four-packets-short-with-a-clean-exit.md",
    },
    Disabled { test: "kill_while_blocked", issue: "issues/kernel/deferred-release-outlives-its-syscall.md" },
    Disabled { test: "latency_wake", issue: "issues/build/latency-wake-reds-on-the-dev-host-at-a-rate.md" },
    Disabled {
        test: "partition_claim_departure",
        issue: "issues/boot-media/partition-claim-departure-exits-clean-with-none-of-its-refusals-said.md",
    },
    Disabled {
        test: "quiesce_dump_holds_the_stopped",
        issue: "issues/kernel/quiesce-dump-holds-the-stopped-reds-wide-with-usb-transport-breaks.md",
    },
    Disabled {
        test: "quiesce_wakes_on_the_last_exit",
        issue: "issues/build/quiesce-wakes-on-the-last-exit-lost-its-serial-ready-beside-other-guests.md",
    },
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
    Disabled {
        test: "usb_disk_index_stable",
        issue: "issues/hardware/usb-disk-index-stable-nothing-enumerates-on-the-first-controller.md",
    },
];

/// The row of `rows` that disables `test`, matched by the whole name.
pub fn disabled<'a>(rows: &'a [Disabled], test: &str) -> Option<&'a Disabled> {
    rows.iter().find(|row| row.test == test)
}

/// Whether `issue` has the one shape a per-test issue is allowed:
/// `issues/<area>/<slug>.md` — an area directory and a file, nothing nested
/// deeper and nothing sitting directly in `issues/` itself (`issues/README.md`
/// is the tracker's own doc, not a test's issue).
fn is_per_test_issue_path(issue: &str) -> bool {
    let Some(rest) = issue.strip_prefix("issues/") else { return false };
    let mut parts = rest.split('/');
    let (Some(_area), Some(slug)) = (parts.next(), parts.next()) else { return false };
    parts.next().is_none() && slug.ends_with(".md")
}

/// Whether the file at `path` opens with a frontmatter block whose `status`
/// field is `expected-red` — the one status a disabled test's issue may carry
/// (`issues/README.md`'s own frontmatter table).
fn is_expected_red(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else { return false };
    let mut lines = text.lines();
    if lines.next() != Some("---") {
        return false;
    }
    for line in lines {
        if line == "---" {
            return false;
        }
        if let Some(value) = line.strip_prefix("status:") {
            return value.trim() == "expected-red";
        }
    }
    false
}

/// Every row of `rows` against the tree under `root`: no test twice, each one
/// `registered`, and each issue an `issues/<area>/<slug>.md` file whose
/// frontmatter says `status: expected-red`.
pub fn check(rows: &[Disabled], registered: impl Fn(&str) -> bool, root: &Path) -> Result<(), String> {
    for (at, row) in rows.iter().enumerate() {
        if disabled(&rows[..at], row.test).is_some() {
            return Err(format!("{} is disabled twice", row.test));
        }
        if !registered(row.test) {
            return Err(format!(
                "{} is disabled and nothing registers it: a renamed or deleted test takes its row \
                 with it",
                row.test
            ));
        }
        if !is_per_test_issue_path(row.issue) {
            return Err(format!(
                "{}: `{}` is not an `issues/<area>/<slug>.md` path",
                row.test, row.issue
            ));
        }
        let path = root.join(row.issue);
        if !path.is_file() {
            return Err(format!("{}: `{}` is not a file in this tree", row.test, row.issue));
        }
        if !is_expected_red(&path) {
            return Err(format!(
                "{}: `{}` does not open with `status: expected-red`",
                row.test, row.issue
            ));
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
    match disabled(rows, test) {
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

    /// A scratch `issues/build/<slug>.md` under `dir`, so a test can point a
    /// row at a file it controls rather than one this tree tracks and might
    /// close out from under it.
    fn write_issue_fixture(dir: &Path, path: &str, status: &str) {
        let full = dir.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, format!("---\nstatus: {status}\nkind: defect\n---\n\n# fixture\n")).unwrap();
    }

    #[test]
    fn every_row_names_one_test_and_an_issue_file_that_exists() {
        check(DISABLED, |_| true, root()).unwrap();
    }

    #[test]
    fn a_row_is_refused_for_an_unregistered_test_a_missing_issue_or_a_second_row() {
        let tmp = toyos_tmpdir::TempDir::new("redlist-check");
        const ISSUE: &str = "issues/build/a-fixture.md";
        write_issue_fixture(tmp.path(), ISSUE, "expected-red");
        let registered = |name: &str| name == "a_real_test";
        check(&[Disabled { test: "a_real_test", issue: ISSUE }], registered, tmp.path()).unwrap();
        check(&[], registered, tmp.path()).unwrap();
        let refused = |rows: &[Disabled], why: &str| {
            let said = check(rows, registered, tmp.path()).unwrap_err();
            assert!(said.contains(why), "{said}");
        };
        refused(&[Disabled { test: "a_renamed_test", issue: ISSUE }], "nothing registers it");
        refused(
            &[Disabled { test: "a_real_test", issue: "issues/build/no-such-issue.md" }],
            "is not a file in this tree",
        );
        refused(
            &[Disabled { test: "a_real_test", issue: "issues/README.md" }],
            "is not an `issues/<area>/<slug>.md` path",
        );
        refused(
            &[Disabled { test: "a_real_test", issue: "CLAUDE.md" }],
            "is not an `issues/<area>/<slug>.md` path",
        );
        refused(
            &[Disabled { test: "a_real_test", issue: ISSUE }, Disabled { test: "a_real_test", issue: ISSUE }],
            "disabled twice",
        );
    }

    #[test]
    fn a_row_is_refused_unless_its_issue_says_status_expected_red() {
        let tmp = toyos_tmpdir::TempDir::new("redlist-check-status");
        let registered = |_: &str| true;
        for (path, status) in [
            ("issues/build/a-fixture-open.md", "open"),
            ("issues/build/a-fixture-assigned.md", "assigned"),
            ("issues/build/a-fixture-owner.md", "owner"),
            ("issues/build/a-fixture-none.md", "none"),
        ] {
            write_issue_fixture(tmp.path(), path, status);
            let said = check(&[Disabled { test: "a_real_test", issue: path }], registered, tmp.path()).unwrap_err();
            assert!(said.contains("does not open with `status: expected-red`"), "{status}: {said}");
        }
        const RED: &str = "issues/build/a-fixture-red.md";
        write_issue_fixture(tmp.path(), RED, "expected-red");
        check(&[Disabled { test: "a_real_test", issue: RED }], registered, tmp.path()).unwrap();
    }

    #[test]
    fn a_row_disables_its_whole_name_and_nothing_that_extends_it() {
        for row in DISABLED {
            assert_eq!(disabled(DISABLED, row.test), Some(row));
            assert_eq!(disabled(DISABLED, &format!("{}_controls", row.test)), None);
        }
        assert_eq!(disabled(DISABLED, ""), None);
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
