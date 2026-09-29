//! Loom: what a target wrote while serving a generation is what an initiator
//! that saw it served reads — the edge a `perf-state` read copies each CPU's
//! registers through. The slot is a loom cell rather than an atomic, so a read
//! unordered against the write is loom's own `Causality violation`.

#![cfg(feature = "loom")]

use std::sync::atomic::{AtomicBool, Ordering::SeqCst};

use kernel_loom::shootdown::Shootdown;
use loom::cell::UnsafeCell;
use loom::sync::Arc;

/// What the target's serve stores.
const SAMPLE: u64 = 0x8000_2a04;

/// Set by any execution in which the initiator saw the answer, so the
/// assertion is shown to have run. Outside the model because loom re-runs the
/// closure once per interleaving.
static SEEN: AtomicBool = AtomicBool::new(false);

struct Machine {
    shootdown: Shootdown,
    /// cpu 1's answer slot.
    slot: UnsafeCell<u64>,
}

// SAFETY: `slot` is written only inside cpu 1's `serve` and read only after
// `served` says that serve finished; that ordering is what the model checks.
unsafe impl Sync for Machine {}

#[test]
fn what_a_serve_wrote_is_read_once_it_is_served() {
    SEEN.store(false, SeqCst);
    loom::model(|| {
        let m = Arc::new(Machine { shootdown: Shootdown::new(), slot: UnsafeCell::new(0) });
        let generation = m.shootdown.issue();

        let target = {
            let m = m.clone();
            loom::thread::spawn(move || {
                // SAFETY: the only write, before the serve's publication.
                m.shootdown.serve(1, || m.slot.with_mut(|slot| unsafe { *slot = SAMPLE }));
            })
        };

        if m.shootdown.served(1, generation) {
            SEEN.store(true, SeqCst);
            // SAFETY: `served` answered, so the write happened before this.
            let read = m.slot.with(|slot| unsafe { *slot });
            assert_eq!(read, SAMPLE, "the initiator saw cpu 1 served and read its slot unwritten");
        }

        target.join().unwrap();
    });
    assert!(SEEN.load(SeqCst), "no interleaving saw the answer, so the assertion never ran");
}
