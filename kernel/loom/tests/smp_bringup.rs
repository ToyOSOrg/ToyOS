//! Loom: AP bring-up's two ordering edges, invisible to a guest test on x86 TSO,
//! and its commit's refusal of an AP that reads its own hardware id as another.
//! `--features roster-commit-relaxed` reds publication (a reader sees a count over
//! an unfilled slot); `--features smp-ready-split` reds release (a CPU sees the
//! machine released but not yet answering, so a shootdown skips a joined AP). The
//! reader is a thread that never read the word first, which would pin loom's
//! coherence order and hide the reorder.

#![cfg(feature = "loom")]

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};

use kernel_loom::arch::cpu::become_cpu;
use kernel_loom::smp_roster::Roster;
use loom::sync::Arc;

/// A hardware id no uncommitted slot holds (the roster fills a slot with `u32::MAX`).
const HARDWARE_ID: u32 = 7;

/// What an AP started as [`HARDWARE_ID`] reads as its own when it reads wrong.
const MISREAD: u32 = 5;

/// Bounded for loom (an unbounded spin never finishes); each turn a scheduling point.
const POLLS: usize = 4;

/// Guards against a bounded exploration passing without reaching the state.
static SAW: AtomicBool = AtomicBool::new(false);

/// A reader that sees a committed count sees the slot it covers.
#[test]
fn a_committed_count_never_outruns_its_slot() {
    SAW.store(false, SeqCst);
    loom::model(|| {
        let r = Arc::new(Roster::new());
        let attempt = r.begin_attempt().expect("a slot is free");
        assert_eq!(attempt.id(), 1, "cpu1 is the first AP");
        become_cpu(HARDWARE_ID);
        r.echo(attempt.token());

        let committer = {
            let r = r.clone();
            loom::thread::spawn(move || r.commit(attempt, HARDWARE_ID))
        };

        let reader = loom::thread::spawn(move || {
            if r.count() >= 2 {
                let got = r.hardware_id(1);
                SAW.store(true, SeqCst);
                assert_eq!(
                    got, HARDWARE_ID,
                    "a reader saw cpu1 counted while its hardware-id slot was still unfilled",
                );
            }
        });

        committer.join().unwrap();
        reader.join().unwrap();
    });
    assert!(
        SAW.load(SeqCst),
        "no interleaving read the grown count, so the assertion never ran",
    );
}

/// A CPU that sees the machine released also sees it answering.
#[test]
fn a_released_machine_is_answering() {
    SAW.store(false, SeqCst);
    loom::model(|| {
        let r = Arc::new(Roster::new());

        let release = {
            let r = r.clone();
            loom::thread::spawn(move || r.release())
        };

        let initiator = loom::thread::spawn(move || {
            for _ in 0..POLLS {
                if r.released() {
                    let answering = r.answering();
                    SAW.store(true, SeqCst);
                    assert!(
                        answering,
                        "a CPU saw the machine released but not answering: a shootdown \
                         here is local-only and skips an AP that may already have joined",
                    );
                    break;
                }
                loom::thread::yield_now();
            }
        });

        release.join().unwrap();
        initiator.join().unwrap();
    });
    assert!(
        SAW.load(SeqCst),
        "no interleaving observed the release, so the assertion never ran",
    );
}

/// The boot CPU that sees an AP's echo refuses to commit it under an id other
/// than the one the AP echoed as its own read.
#[test]
#[should_panic(
    expected = "smp: cpu1 reads its own hardware id as 0x5, and its roster slot and every IPI name it 0x7"
)]
fn an_ap_that_reads_another_id_is_refused() {
    loom::model(|| {
        let r = Arc::new(Roster::new());
        let attempt = r.begin_attempt().expect("a slot is free");

        let ap = {
            let r = r.clone();
            loom::thread::spawn(move || {
                become_cpu(MISREAD);
                r.echo(attempt.token());
            })
        };

        let polls = Cell::new(0);
        let echoed = r.await_echo(attempt, || {
            polls.set(polls.get() + 1);
            loom::thread::yield_now();
            polls.get() > POLLS
        });
        ap.join().unwrap();
        if echoed {
            r.commit(attempt, HARDWARE_ID);
        }
    });
}
