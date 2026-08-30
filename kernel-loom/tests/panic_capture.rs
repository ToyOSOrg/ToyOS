//! Loom: the panic snapshot's capture latch — one writer per `SNAPSHOT`.
//!
//! Two CPUs panicking at once both hold `cli`, so nothing else serialises
//! `capture()`'s render into the one static; the latch makes the first captor
//! its only writer, re-entrant for `refresh_capture`, released on recovery.
//! The negative control is in this file: the unlatched shape, transliterated.

use std::sync::atomic::{AtomicBool, Ordering as StdOrdering};

use kernel_loom::capture_latch::CaptureLatch;
use loom::cell::UnsafeCell;
use loom::sync::atomic::{AtomicU32, Ordering};
use loom::sync::Arc;
use loom::thread;

/// One writer per snapshot, from every interleaving of two panicking CPUs; the
/// two-byte cell stands for `SNAPSHOT`, and a mixed pair is two reports on one screen.
#[test]
fn two_panicking_cpus_write_one_snapshot() {
    loom::model(|| {
        let latch = Arc::new(CaptureLatch::new());
        let snapshot = Arc::new(UnsafeCell::new([0u8; 2]));

        let mut captors = Vec::new();
        for token in [2u32, 3u32] {
            let (latch, snapshot) = (latch.clone(), snapshot.clone());
            captors.push(thread::spawn(move || {
                if latch.claim(token) {
                    // SAFETY: loom instruments the cell; a second writer is the defect under test.
                    snapshot.with_mut(|p| unsafe { (*p)[0] = token as u8 });
                    snapshot.with_mut(|p| unsafe { (*p)[1] = token as u8 });
                }
            }));
        }
        for captor in captors {
            captor.join().unwrap();
        }

        // SAFETY: both writers joined; this read races nothing.
        let written = snapshot.with(|p| unsafe { *p });
        assert!(
            written == [2, 2] || written == [3, 3],
            "the snapshot carries two interleaved reports: {written:?}",
        );
    });
}

/// The owner's `refresh_capture` re-enters while a second captor is turned
/// away, and the refresh lands whole over the owner's first write.
#[test]
fn the_owner_refreshes_and_a_second_captor_stays_out() {
    loom::model(|| {
        let latch = Arc::new(CaptureLatch::new());
        let snapshot = Arc::new(UnsafeCell::new(0u32));

        assert!(latch.claim(2), "an unclaimed latch refused its first captor");
        // SAFETY: claim(2) held; the loser thread below is refused before it writes.
        snapshot.with_mut(|p| unsafe { *p = 2 });

        let loser = {
            let (latch, snapshot) = (latch.clone(), snapshot.clone());
            thread::spawn(move || {
                if latch.claim(3) {
                    // SAFETY: reachable only when the latch wrongly admits a second captor.
                    snapshot.with_mut(|p| unsafe { *p = 3 });
                }
            })
        };

        assert!(latch.claim(2), "the owner was refused its own refresh");
        // SAFETY: same claim as the first write.
        snapshot.with_mut(|p| unsafe { *p = 22 });

        loser.join().unwrap();
        // SAFETY: the loser joined; this read races nothing.
        let written = snapshot.with(|p| unsafe { *p });
        assert_eq!(written, 22, "a refused captor's write survived: {written}");
    });
}

/// A recovered panic releases the latch, and the next captor's write is
/// ordered after the last owner's — `claim`'s acquire against `release`'s store.
#[test]
fn a_recovered_panic_hands_the_snapshot_to_the_next_captor() {
    loom::model(|| {
        let latch = Arc::new(CaptureLatch::new());
        let snapshot = Arc::new(UnsafeCell::new(0u32));

        assert!(latch.claim(2), "an unclaimed latch refused its first captor");
        // SAFETY: claim(2) held until the release below.
        snapshot.with_mut(|p| unsafe { *p = 2 });
        latch.release();

        let next = {
            let snapshot = snapshot.clone();
            thread::spawn(move || {
                assert!(latch.claim(3), "a released latch refused its next captor");
                // SAFETY: the previous owner released before this thread was spawned.
                snapshot.with_mut(|p| unsafe { *p = 3 });
            })
        };
        next.join().unwrap();

        // SAFETY: the next captor joined; this read races nothing.
        assert_eq!(snapshot.with(|p| unsafe { *p }), 3);
    });
}

/// The negative control: the unlatched shape, transliterated onto two atomic
/// words. Asserted by collecting a mixed pair rather than by failing, so it
/// stays a passing test that reds the day loom stops reaching the interleaving
/// the models above rest on.
#[test]
fn the_unlatched_capture_this_replaced_interleaves_two_reports() {
    static MIXED: AtomicBool = AtomicBool::new(false);

    loom::model(|| {
        let snapshot = Arc::new((AtomicU32::new(0), AtomicU32::new(0)));

        let mut captors = Vec::new();
        for token in [2u32, 3u32] {
            let snapshot = snapshot.clone();
            captors.push(thread::spawn(move || {
                // `capture()` as it was: no claim, straight into the render.
                snapshot.0.store(token, Ordering::Relaxed);
                snapshot.1.store(token, Ordering::Relaxed);
            }));
        }
        for captor in captors {
            captor.join().unwrap();
        }

        let written = (snapshot.0.load(Ordering::Relaxed), snapshot.1.load(Ordering::Relaxed));
        if written.0 != written.1 {
            MIXED.store(true, StdOrdering::Relaxed);
        }
    });

    assert!(
        MIXED.load(StdOrdering::Relaxed),
        "no interleaving mixed the two reports, so these models cannot tell the unlatched \
         shape from the latched one — the models above are passing on an exploration that \
         is not happening",
    );
}
