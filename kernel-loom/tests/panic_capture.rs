//! Loom: the panic snapshot's capture latch.
//!
//! Two CPUs panicking at once both take `cli` first, so neither takes the
//! other's halt IPI, and both reach `panic_console::capture()` — which renders
//! the log's tail into the one static `SNAPSHOT`. Nothing else serialises that
//! write: `PAINTING` gates the painters, not the captors, and the screen could
//! carry two interleaved reports. The latch makes the first captor the
//! snapshot's only writer, re-entrant for the refresh `wait_for_log_file`
//! owes, released only when a recovered panic gives the snapshot back.
//!
//! A rate is why this is a model: the window is one render on one CPU against
//! another CPU entering its own panic, which no guest boot lands in on demand.
//! The negative control is in this file — the unlatched shape, transliterated,
//! whose interleaved snapshot loom must actually reach.

use std::sync::atomic::{AtomicBool, Ordering as StdOrdering};

use kernel_loom::capture_latch::CaptureLatch;
use loom::cell::UnsafeCell;
use loom::sync::atomic::{AtomicU32, Ordering};
use loom::sync::Arc;
use loom::thread;

/// The snapshot has one writer, from every interleaving of two panicking CPUs.
///
/// The two-byte cell stands for `SNAPSHOT`: a render is many writes, and two
/// are enough for loom to observe an interleaving. The final content must be
/// wholly one captor's — a mixed pair is two reports on one screen.
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

/// `refresh_capture` re-enters on the owning CPU while a second panicking CPU
/// is turned away — the refresh is a second write by the same owner, and it
/// must land, whole, over the owner's first.
#[test]
fn the_owner_refreshes_and_a_second_captor_stays_out() {
    loom::model(|| {
        let latch = Arc::new(CaptureLatch::new());
        let snapshot = Arc::new(UnsafeCell::new(0u32));

        // The first capture, before the second CPU panics.
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

        // The refresh: a line written after capture(), folded into the snapshot.
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
/// ordered after everything the last owner wrote — the acquire on `claim`
/// pairing with `release`'s store is what loom checks here.
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

/// The unlatched shape this replaced, transliterated: both captors render into
/// the same cells with nothing serialising them. Two atomic words stand in for
/// `SNAPSHOT`'s bytes so the interleaving is a readable value rather than the
/// causality panic loom raises over an `UnsafeCell`.
///
/// Asserted by collecting rather than by failing, so this stays a passing test
/// that proves the failure is reachable: if a future loom, or an edit to the
/// models above, stops exploring the window in which two renders interleave,
/// **this reds** — at that point the models above pass on an exploration that
/// is not happening.
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
