//! The quarantine list: the tests known to fail on a defect somebody owns.
//!
//! A quarantined test still runs on every run, and its failure is reported by
//! name and does not fail the suite. A failing test that is not on this list
//! fails it. `tests/toyos.rs` reads [`quarantined`] for every verdict and
//! refuses a row whose name nothing registers.
//!
//! `cargo run -- --known-red <test>` answers yes or no with the reason; with no
//! argument it prints the list.

use crate::flags;

/// One quarantined test.
#[derive(PartialEq, Eq, Debug)]
pub struct Quarantined {
    /// The registered test name, exactly.
    pub test: &'static str,
    /// What the failure says, in one line.
    pub reason: &'static str,
    /// The issue file that owns the defect. Closing the issue deletes the row.
    pub issue: &'static str,
}

/// Every quarantined test, by name.
pub const QUARANTINE: &[Quarantined] = &[
    Quarantined {
        test: "console_line_atomicity",
        reason: "`writer A declared 1000 whole lines and the capture carries 995`",
        issue: "issues/build/parallel-tests-red-under-other-suites.md",
    },
    Quarantined {
        test: "console_locale_detect",
        reason: "`STALLED: waiting for the wizard to ask for a key under /system/bin/console — \
                 the console did not lend it the keyboard — it never stopped talking and never \
                 got there`",
        issue: "issues/build/the-console-input-path-can-stop-after-a-ps2-overflow.md",
    },
    Quarantined {
        test: "desktop_window_child",
        reason: "`the windowed child never reported leaving`",
        issue: "issues/kernel/desktop-window-child-freeze.md",
    },
    Quarantined {
        test: "doom_sound_flood",
        reason: "`the device played a peak of 32768 (expected 4000..=12000): the volume the last \
                 command named is not the volume that reached the wire`",
        issue: "issues/audio/doom-sound-flood-played-full-scale-once.md",
    },
    Quarantined {
        test: "fs_dirs_durable",
        reason: "`the staged directories left the log volume breaking the format`",
        issue: "issues/build/a-loaded-suite-reds-a-volume-checker-on-both-arms.md",
    },
    Quarantined {
        test: "handle_transfer",
        reason: "`handle transfer left more live objects behind: [(\"PipeRead\", 2, 3)]`",
        issue: "issues/kernel/deferred-release-outlives-its-syscall.md",
    },
    Quarantined {
        test: "hda_tone",
        reason: "`the captured tone is not one sine`",
        issue: "issues/audio/hda-tone-phase-check.md",
    },
    Quarantined {
        test: "i8042_undecoded_bytes",
        reason: "`the verdict was said too early` and never revised: no later `nothing decoded` \
                 line names the sequence",
        issue: "issues/build/parallel-tests-red-under-other-suites.md",
    },
    Quarantined {
        test: "kernel_log_file",
        reason: "`logd never opened a file`",
        issue: "issues/boot-media/kernel-log-file-reds-beside-other-guests-and-is-green-alone.md",
    },
    Quarantined {
        test: "kill_while_blocked",
        reason: "`a pipe whose only reader was killed mid-read still took a write` (arm 1) once, \
                 and `a connection whose peer was killed mid-read still took a write` (arm 2) once",
        issue: "issues/kernel/deferred-release-outlives-its-syscall.md",
    },
    Quarantined {
        test: "latency_wake",
        reason: "`the p99 landed in the histogram's last bucket, so 4096us is a floor and not a \
                 measurement`",
        issue: "issues/build/latency-wake-reds-on-the-dev-host-at-a-rate.md",
    },
    Quarantined {
        test: "launcher_refusals",
        reason: "`launcher_refusals exited Some(101)` on the assertion, not on a ceiling: `16 more \
                 refused launches left more live objects behind`",
        issue: "issues/build/parallel-tests-red-under-other-suites.md",
    },
    Quarantined {
        test: "leak_rollback_selftest",
        reason: "`leak-selftest: fat-reopen skipped, create failed: WouldBlock`",
        issue: "issues/kernel/leak-rollback-selftest-create-answers-wouldblock.md",
    },
    Quarantined {
        test: "log_poll_outlives_a_close",
        reason: "`the close probe exited Some(1)`",
        issue: "issues/panic-path/a-double-panic-at-boots-edge-says-nothing-but-its-name.md",
    },
    Quarantined {
        test: "root_named_but_absent",
        reason: "`the kernel did not refuse this ROOT set`",
        issue: "issues/boot-media/root-named-but-absent-misses-the-refusal-inside-its-window.md",
    },
    Quarantined {
        test: "sched_check_build",
        reason: "a pass-cost distribution with mass over the 200000 ns budget that the KVM, \
                 native x86-64 sample never showed",
        issue: "issues/build/the-pass-cost-gates-ci-sample-is-eight-days-stale-twice.md",
    },
    Quarantined {
        test: "sched_stress",
        reason: "`log-gate: FAILED: cpu5 seq 517 is stamped 650224011 ns, behind the 651750439 ns \
                 of the record before it`",
        issue: "issues/diagnostics/a-shards-timestamps-run-backwards-at-seq-517.md",
    },
    Quarantined {
        test: "screen_fatal_halt",
        reason: "`[qemu] Boot timed out waiting for ===READY===`, with a usb-storage transport \
                 break during boot",
        issue: "issues/boot-media/screen-fatal-halt-reds-on-ci-with-a-usb-storage-transport-break-during-boot.md",
    },
    Quarantined {
        test: "short_sleep_livelock",
        reason: "`STALLED: 63s of guard expired, and the guest had said nothing for the last 63s \
                 of it`",
        issue: "issues/kernel/short-sleep-livelock-stalls-on-ci-with-one-sleeper-never-returning.md",
    },
    Quarantined {
        test: "so_cache_refusals",
        reason: "no \"byte budget; refused\" line — the kernel refused nothing",
        issue: "issues/kernel/so-cache-refusals-saw-the-kernel-refuse-nothing-once.md",
    },
    Quarantined {
        test: "syscall_window_nmi",
        reason: "`the storm never reported — is `syscall-window-nmi` on?`",
        issue: "issues/build/parallel-tests-red-under-other-suites.md",
    },
    Quarantined {
        test: "usb_disk_index_stable",
        reason: "`nothing enumerated on the first controller; there is no renumbering to survive`",
        issue: "issues/hardware/eleven-names-red-on-ci.md",
    },
];

/// The row for `test`, if it is quarantined.
pub fn quarantined(test: &str) -> Option<&'static Quarantined> {
    QUARANTINE.iter().find(|q| q.test == test)
}

/// `cargo run -- --known-red [<test>]`.
pub fn dispatch(args: &[String]) {
    print!("{}", answer(QUARANTINE, flags::CARGO_RUN.value(args, &flags::KNOWN_RED)));
}

fn answer(rows: &[Quarantined], asked: Option<&str>) -> String {
    let Some(test) = asked else {
        return rows.iter().map(|q| format!("{}  {}  {}\n", q.test, q.issue, q.reason)).collect();
    };
    match rows.iter().find(|q| q.test == test) {
        Some(q) => format!(
            "{test}: YES, quarantined — its failure does not fail the suite.\n  {}\n  {}\n",
            q.reason, q.issue
        ),
        None => format!("{test}: NO, not quarantined — its failure fails the suite.\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::path::Path;

    #[test]
    fn every_row_names_one_test_a_reason_and_an_issue_file_that_exists() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut seen = BTreeSet::new();
        for q in QUARANTINE {
            assert!(seen.insert(q.test), "{} is quarantined twice", q.test);
            assert!(!q.reason.trim().is_empty(), "{} gives no reason", q.test);
            assert!(
                q.issue.starts_with("issues/") && root.join(q.issue).is_file(),
                "{}: `{}` is not an issue file in this tree",
                q.test,
                q.issue
            );
        }
    }

    #[test]
    fn the_answer_is_yes_or_no_with_the_reason() {
        let rows = [Quarantined { test: "reds", reason: "it said no", issue: "issues/x.md" }];
        let yes = answer(&rows, Some("reds"));
        assert!(yes.starts_with("reds: YES"), "{yes}");
        assert!(yes.contains("it said no") && yes.contains("issues/x.md"), "{yes}");
        let no = answer(&rows, Some("greens"));
        assert!(no.starts_with("greens: NO"), "{no}");
        assert_eq!(answer(&rows, None).lines().count(), 1);
    }
}
