//! Loom: the two ordering edges of AP bring-up.
//!
//! Both properties are invisible to every guest test — x86's TSO gives each
//! store release and each load acquire semantics, so a relaxed edge here behaves
//! identically on the only architecture ToyOS boots. Each edge is a cargo
//! feature the kernel never enables and this model must red under.
//!
//! **Publication.** A CPU that reads a count reads every slot the count covers:
//!
//! ```text
//! cargo test --manifest-path kernel-loom/Cargo.toml --features roster-commit-relaxed \
//!   --test smp_bringup
//! ```
//!
//! makes `commit` publish the count relaxed and
//! [`a_committed_count_never_outruns_its_slot`] reds — a reader sees a count over
//! a slot the commit has not filled, which in the kernel is a shootdown waiting
//! on, or an IPI resolving through, a cpu id whose LAPIC mapping is not there yet.
//!
//! **Release.** Releasing the APs and answering their shootdowns are one store,
//! so no CPU sees itself released while a shootdown still skips it:
//!
//! ```text
//! cargo test --manifest-path kernel-loom/Cargo.toml --features smp-ready-split \
//!   --test smp_bringup
//! ```
//!
//! grows the base's second store back and [`a_released_machine_is_answering`]
//! reds — a CPU observes the machine released, so an AP may have joined and be
//! holding translations, while `answering` is still false and a shootdown there
//! takes the local-only branch that skips it.
//!
//! Each assertion is guarded by a static the winning interleaving sets, so a
//! bounded exploration that never reached the interesting state cannot pass
//! vacuously — the same guard `tlb_shootdown.rs` documents. The reader is a
//! thread that has not itself touched the word it observes: a committer that
//! reserved the id first, or a reader that read the release word first, pins
//! loom's own view of the coherence order and would hide the very reorder the
//! control stages.

#![cfg(feature = "loom")]

use std::sync::atomic::{AtomicBool, Ordering::SeqCst};

use kernel_loom::smp_roster::Roster;
use loom::sync::Arc;

/// A LAPIC id no uncommitted slot holds; the roster fills a slot with `NO_LAPIC`
/// (`u32::MAX`) until it commits, so reading this back proves the slot is filled.
const LAPIC: u32 = 7;

/// How many times the release reader looks before giving up. Bounded for loom's
/// reason — an unbounded spin is an unbounded branch it never finishes — and each
/// turn is a scheduling point at which the release store can land.
const POLLS: usize = 4;

/// Set by any interleaving that actually reached the state under test.
static SAW: AtomicBool = AtomicBool::new(false);

/// A reader that sees a committed count sees the slot it covers.
///
/// The BSP reserves cpu1's id, a committer thread fills and publishes it, and a
/// separate reader thread — one that never read the count itself — observes the
/// count and, if it has grown, the slot behind it.
#[test]
fn a_committed_count_never_outruns_its_slot() {
    SAW.store(false, SeqCst);
    loom::model(|| {
        let r = Arc::new(Roster::new());
        let attempt = r.begin_attempt().expect("a slot is free");
        assert_eq!(attempt.id(), 1, "cpu1 is the first AP");

        let committer = {
            let r = r.clone();
            loom::thread::spawn(move || r.commit(attempt, LAPIC))
        };

        let reader = {
            let r = r.clone();
            loom::thread::spawn(move || {
                if r.count() >= 2 {
                    let got = r.apic_id(1);
                    SAW.store(true, SeqCst);
                    assert_eq!(
                        got, LAPIC,
                        "a reader saw cpu1 counted while its LAPIC slot was still unfilled",
                    );
                }
            })
        };

        committer.join().unwrap();
        reader.join().unwrap();
    });
    assert!(
        SAW.load(SeqCst),
        "no interleaving read the grown count, so the assertion never ran",
    );
}

/// A CPU that sees the machine released also sees it answering.
///
/// One thread releases; a reader thread is a shootdown initiator that only
/// decides its branch once it has observed the release. In the kernel an AP that
/// has observed the release may already have joined and be holding translations,
/// so an initiator here that found `answering` false would take the local-only
/// branch and never flush it.
#[test]
fn a_released_machine_is_answering() {
    SAW.store(false, SeqCst);
    loom::model(|| {
        let r = Arc::new(Roster::new());

        let release = {
            let r = r.clone();
            loom::thread::spawn(move || r.release())
        };

        let initiator = {
            let r = r.clone();
            loom::thread::spawn(move || {
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
            })
        };

        release.join().unwrap();
        initiator.join().unwrap();
    });
    assert!(
        SAW.load(SeqCst),
        "no interleaving observed the release, so the assertion never ran",
    );
}
