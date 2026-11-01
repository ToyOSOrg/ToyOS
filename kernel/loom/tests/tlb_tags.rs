//! Loom: a change to a space's leaves reaches every CPU that could still use
//! the old translation, or that CPU flushes before it uses one.
//!
//! cpu 1 ran the space before (its TLB holds the mapped entry) and enters it
//! again while cpu 0 unmaps the page and asks who runs the space. Either
//! cpu 0's [`Tags::changed`] names cpu 1, which then owes a flush before the
//! page is freed, or cpu 1's [`Tags::enter`] answers that it must flush. The
//! model fails if neither holds and cpu 1 reads the page through the stale
//! entry.
//!
//! ```text
//! cargo test --manifest-path kernel/loom/Cargo.toml --features tlb-tags-enter-relaxed \
//!   --test tlb_tags
//! ```
//!
//! relaxes `enter`'s pairing, and this file must red.

#![cfg(feature = "loom")]

use kernel_loom::tlb_tags::Tags;
use loom::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};
use loom::sync::Arc;

const TAG: usize = 1;

#[test]
fn a_change_reaches_a_runner_or_the_entering_cpu_flushes() {
    loom::model(|| {
        let tags = Arc::new(Tags::new());
        tags.size(2, 2);
        let pte = Arc::new(AtomicU64::new(1));
        let reached = Arc::new(AtomicBool::new(false));

        let initiator = {
            let (tags, pte, reached) = (Arc::clone(&tags), Arc::clone(&pte), Arc::clone(&reached));
            loom::thread::spawn(move || {
                pte.store(0, SeqCst);
                tags.changed(TAG, |cpu| {
                    if cpu == 1 {
                        reached.store(true, SeqCst);
                    }
                });
            })
        };

        // cpu 1's TLB still holds the mapped entry from its last run.
        let mut tlb = 1;
        if tags.switch(1, TAG).flush {
            let stamp = tags.flushing();
            tlb = pte.load(SeqCst);
            tags.flushed(1, stamp);
        }
        let used = tlb;
        initiator.join().unwrap();

        // A reached cpu flushes before the frame is freed, which is the
        // shootdown protocol's own property (`tlb_shootdown.rs`).
        assert!(
            reached.load(SeqCst) || used == pte.load(SeqCst),
            "cpu 1 entered the space unreached and used a translation the change took away"
        );
    });
}
