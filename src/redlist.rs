//! The quarantine list: the tests known to fail on a defect somebody owns.
//!
//! A row excuses one known failure, not the test. A quarantined test still runs
//! on every run; a failure whose text contains one of the row's
//! [`says`](Quarantined::says) fragments is reported by name and does not fail
//! the suite, and the same test failing any other way fails it like a test on no
//! list. `tests/toyos.rs` reads [`QUARANTINE`] for every verdict and refuses a
//! row whose name nothing registers.
//!
//! `cargo run -- --known-red <test>` answers yes or no with what the row
//! excuses; with no argument it prints the list.

use crate::flags;

/// One quarantined failure of one test.
#[derive(PartialEq, Eq, Debug)]
pub struct Quarantined {
    /// The registered test name, exactly.
    pub test: &'static str,
    /// The failure this row excuses, quoted from the test's own message: the
    /// failure's text must contain one of these or the row does not apply.
    /// Alternatives rather than conjuncts, because one defect can surface at
    /// more than one of a test's assertions. A quotation, so that a reworded
    /// assertion stops matching and the run reds asking about it — of the test's
    /// own message and never of the harness's framing around it, which a gate
    /// below refuses by name.
    pub says: &'static [&'static str],
    /// The issue file that owns the defect.
    pub issue: &'static str,
}

impl Quarantined {
    /// Whether `failure` is the failure this row is about.
    pub fn excuses(&self, failure: &str) -> bool {
        self.says.iter().any(|fragment| failure.contains(fragment))
    }
}

/// Every quarantined failure, by test name.
pub const QUARANTINE: &[Quarantined] = &[
    Quarantined {
        test: "console_line_atomicity",
        says: &["whole lines and the capture carries"],
        issue: "issues/build/parallel-tests-red-under-other-suites.md",
    },
    Quarantined {
        test: "console_locale_detect",
        says: &["waiting for the wizard to ask for a key under /system/bin/console — the console \
                 did not lend it the keyboard — it never stopped talking and never got there"],
        issue: "issues/build/the-console-input-path-can-stop-after-a-ps2-overflow.md",
    },
    Quarantined {
        test: "desktop_window_child",
        says: &[
            "the windowed child never reported leaving",
            "a windowed child exited by itself and the shell never answered again",
            "GUI+Q never reached the compositor",
            "the compositor closed the window and the client did not leave",
            "snake did not leave when its window was closed in round",
            "snake's window was closed, snake left, and the shell never answered again",
        ],
        issue: "issues/kernel/desktop-window-child-freeze.md",
    },
    Quarantined {
        test: "doom_sound_flood",
        says: &["the device played a peak of 32768"],
        issue: "issues/audio/doom-sound-flood-played-full-scale-once.md",
    },
    Quarantined {
        test: "handle_transfer",
        says: &["handle transfer left more live objects behind"],
        issue: "issues/kernel/deferred-release-outlives-its-syscall.md",
    },
    Quarantined {
        test: "hda_tone",
        says: &["the captured tone is not one sine"],
        issue: "issues/audio/hda-tone-phase-check.md",
    },
    Quarantined {
        test: "kill_while_blocked",
        says: &[
            "a pipe whose only reader was killed mid-read still took a write",
            "a connection whose peer was killed mid-read still took a write",
        ],
        issue: "issues/kernel/deferred-release-outlives-its-syscall.md",
    },
    Quarantined {
        test: "latency_wake",
        says: &["the p99 landed in the histogram's last bucket"],
        issue: "issues/build/latency-wake-reds-on-the-dev-host-at-a-rate.md",
    },
    Quarantined {
        test: "sched_check_build",
        says: &["this distribution has mass the KVM, native x86-64 sample never showed"],
        issue: "issues/build/the-pass-cost-gates-ci-sample-is-eight-days-stale-twice.md",
    },
    Quarantined {
        test: "screen_fatal_halt",
        says: &["transport broke on SCSI 0x35"],
        issue: "issues/boot-media/screen-fatal-halt-reds-on-ci-with-a-usb-storage-transport-break-during-boot.md",
    },
    Quarantined {
        test: "short_sleep_livelock",
        says: &["of guard expired, and the guest had said nothing for the last"],
        issue: "issues/kernel/short-sleep-livelock-stalls-on-ci-with-one-sleeper-never-returning.md",
    },
    Quarantined {
        test: "so_cache_refusals",
        says: &["no \"byte budget; refused\" line — the kernel refused nothing"],
        issue: "issues/kernel/so-cache-refusals-saw-the-kernel-refuse-nothing-once.md",
    },
    Quarantined {
        test: "usb_disk_index_stable",
        says: &["nothing enumerated on the first controller; there is no renumbering to survive"],
        issue: "issues/hardware/eleven-names-red-on-ci.md",
    },
];

/// `cargo run -- --known-red [<test>]`.
pub fn dispatch(args: &[String]) {
    print!("{}", answer(QUARANTINE, flags::CARGO_RUN.value(args, &flags::KNOWN_RED)));
}

fn answer(rows: &[Quarantined], asked: Option<&str>) -> String {
    let excused = |q: &Quarantined| {
        q.says.iter().map(|fragment| format!("{fragment:?}")).collect::<Vec<_>>().join(" or ")
    };
    let Some(test) = asked else {
        return rows.iter().map(|q| format!("{}  {}  {}\n", q.test, q.issue, excused(q))).collect();
    };
    match rows.iter().find(|q| q.test == test) {
        Some(q) => format!(
            "{test}: YES, quarantined — a failure saying {} does not fail the suite, and any \
             other failure of it does.\n  {}\n",
            excused(q),
            q.issue
        ),
        None => format!("{test}: NO, not quarantined — its failure fails the suite.\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::path::Path;

    /// How the harness frames a shared-boot failure the guest left no message
    /// for: it heads every assertion of every such test, so a row quoting it —
    /// or quoting any part of it — would excuse the test rather than a failure.
    const FRAMING: &str = "exit code Some(";

    #[test]
    fn every_row_names_one_test_a_failure_and_an_issue_file_that_exists() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut seen = BTreeSet::new();
        for q in QUARANTINE {
            assert!(seen.insert(q.test), "{} is quarantined twice", q.test);
            assert!(
                !q.says.is_empty(),
                "{} quotes no failure, so its row would excuse every failure or none",
                q.test
            );
            for fragment in q.says {
                assert!(
                    !fragment.trim().is_empty(),
                    "{} carries an empty fragment, which every failure text contains",
                    q.test
                );
                assert!(
                    !fragment.contains(FRAMING) && !FRAMING.contains(fragment),
                    "{}: {fragment:?} is the harness's framing {FRAMING:?} and not an assertion \
                     of the test, so the row would excuse every failure of it",
                    q.test
                );
            }
            assert!(
                q.issue.starts_with("issues/") && root.join(q.issue).is_file(),
                "{}: `{}` is not an issue file in this tree",
                q.test,
                q.issue
            );
        }
    }

    #[test]
    fn a_row_excuses_the_failure_it_quotes_and_no_other() {
        let row = Quarantined {
            test: "reds",
            says: &["it said no", "it said nothing"],
            issue: "issues/x.md",
        };
        assert!(row.excuses("round 2: it said no:\n<log>"));
        assert!(row.excuses("it said nothing"));
        assert!(!row.excuses("the client binary was not built"));
        let quotes_nothing = Quarantined { test: "reds", says: &[], issue: "issues/x.md" };
        assert!(!quotes_nothing.excuses("it said no"));
    }

    #[test]
    fn the_answer_is_yes_or_no_with_what_is_excused() {
        let rows = [Quarantined { test: "reds", says: &["it said no"], issue: "issues/x.md" }];
        let yes = answer(&rows, Some("reds"));
        assert!(yes.starts_with("reds: YES"), "{yes}");
        assert!(yes.contains("it said no") && yes.contains("issues/x.md"), "{yes}");
        let no = answer(&rows, Some("greens"));
        assert!(no.starts_with("greens: NO"), "{no}");
        assert_eq!(answer(&rows, None).lines().count(), 1);
    }
}
