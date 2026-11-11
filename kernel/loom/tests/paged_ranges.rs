//! Loom: a mapped object's node leaves its list only after every range of
//! the space let go of it, each having cleared its leaves first.
//!
//! A space's node of a mapped object is held by each of its ranges of the
//! object and by a fault parked on one of its pages, which holds no space
//! lock. Whoever lets go last unlinks the node, and from then on a walk of
//! the object's mappers does not visit the space: it frees a revoked frame
//! once its pass is done. So the last to let go must see every other's
//! clearing of its leaves, or a frame is freed under a leaf that may still
//! reach it. A leaf here is one cell; `CLEARED` read by the last is the
//! property, and loom reports a read the clearing does not happen before as a
//! causality violation.
//!
//! ```text
//! cargo test --manifest-path kernel/loom/Cargo.toml --features ranges-less-relaxed \
//!   --test paged_ranges
//! ```
//!
//! relaxes the count's letting go, and this file must red.

#![cfg(feature = "loom")]

use kernel_loom::paged_ranges::Ranges;
use loom::cell::UnsafeCell;
use loom::sync::Arc;

const MAPPED: u64 = 1;
const CLEARED: u64 = 0;

/// What the last to let go does before the node leaves the list: the walk
/// it stops visiting will free whatever this leaf maps.
fn unlink(leaf: &UnsafeCell<u64>) {
    // SAFETY: loom checks the access; the model's claim is that it is ordered.
    let seen = leaf.with(|leaf| unsafe { *leaf });
    assert_eq!(seen, CLEARED, "the node left its list while a leaf of its space still mapped the page");
}

/// An unmap in the space clears its leaf and lets its range go; a fault
/// parked on the range's page lets its hold go as it wakes. Either may be
/// last.
#[test]
fn the_last_to_let_go_sees_every_leaf_cleared() {
    loom::model(|| {
        let ranges = Arc::new(Ranges::one());
        ranges.more();
        let leaf = Arc::new(UnsafeCell::new(MAPPED));

        let unmap = {
            let (ranges, leaf) = (ranges.clone(), leaf.clone());
            loom::thread::spawn(move || {
                // SAFETY: the space's lock is the unmapper's; loom checks it.
                leaf.with_mut(|leaf| unsafe { *leaf = CLEARED });
                if ranges.less() {
                    unlink(&leaf);
                }
            })
        };
        let parked = {
            let (ranges, leaf) = (ranges.clone(), leaf.clone());
            loom::thread::spawn(move || {
                if ranges.less() {
                    unlink(&leaf);
                }
            })
        };
        unmap.join().unwrap();
        parked.join().unwrap();
    });
}
