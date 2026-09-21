//! Loom: the one shutdown a boot gets.
//!
//! Two callers of the reset on two CPUs, and exactly one of them is let in: the
//! second one's sweep would band the first wherever it parks inside its own
//! sync. No boot judge reaches this edge — `quiesce_refuses_a_second_shutdown`'s
//! second caller arrives a millisecond after the first, so a claim made of a
//! read and then a write passes it on every boot — and on any architecture the
//! window is two instructions wide. The negative case is a cargo feature:
//!
//! ```text
//! cargo test --manifest-path kernel-loom/Cargo.toml --features shutdown-claim-split \
//!   --test shutdown_claim
//! ```
//!
//! makes `claim.rs`'s exchange a load and a store, and this file must red.

#![cfg(feature = "loom")]

use kernel_loom::shutdown_claim::Claim;
use loom::sync::atomic::{AtomicUsize, Ordering};
use loom::sync::Arc;

#[test]
fn two_callers_at_once_and_one_shutdown() {
    loom::model(|| {
        let claim = Arc::new(Claim::new());
        let let_in = Arc::new(AtomicUsize::new(0));

        let other = {
            let (claim, let_in) = (claim.clone(), let_in.clone());
            loom::thread::spawn(move || {
                if claim.take() {
                    let_in.fetch_add(1, Ordering::Relaxed);
                }
            })
        };
        if claim.take() {
            let_in.fetch_add(1, Ordering::Relaxed);
        }
        other.join().unwrap();

        assert_eq!(let_in.load(Ordering::Relaxed), 1, "two callers ran the shutdown");
        assert!(!claim.take(), "and a third is refused for good");
    });
}
