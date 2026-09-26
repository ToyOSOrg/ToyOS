//! Loom: **a poll is answered once** — the decision a user poll ring's entry
//! makes when the object's post, its registrant's own recheck and a newer poll
//! on the same handle all reach it at the same instant.
//!
//! The invariant, in one sentence: **of every caller that tries to take a poll,
//! at most one succeeds, and a poll taken back is never answered.** Two answers
//! are a completion the process never asked for, written into a ring on
//! somebody else's post; an answer after the withdrawal is a completion for a
//! poll the process already replaced.
//!
//! The negative case is a cargo feature:
//!
//! ```text
//! cargo test --manifest-path kernel-loom/Cargo.toml --features poll-fire-load-store \
//!   --test poll_once
//! ```
//!
//! splits the exchange into a load and a store, and this file must red.
//!
//! Every side is a spawned thread, for the reason `log_wake.rs` gives: loom runs
//! the model's own thread first, so a side written there never sees the others
//! not having run.

#![cfg(feature = "loom")]

use kernel_loom::poll_once::Once;
use loom::sync::atomic::{AtomicU32, Ordering};
use loom::sync::Arc;

/// A post and the registrant's recheck race to answer one armed poll: exactly
/// one of them does.
#[test]
fn a_post_and_a_recheck_answer_a_poll_once() {
    loom::model(|| {
        let poll = Arc::new(Once::new());
        let answers = Arc::new(AtomicU32::new(0));

        let racers: Vec<_> = (0..2)
            .map(|_| {
                let poll = poll.clone();
                let answers = answers.clone();
                loom::thread::spawn(move || {
                    if poll.fire() {
                        answers.fetch_add(1, Ordering::AcqRel);
                    }
                })
            })
            .collect();
        for racer in racers {
            racer.join().unwrap();
        }

        assert_eq!(answers.load(Ordering::Acquire), 1, "a ready poll is answered exactly once");
        assert!(!poll.armed());
    });
}

/// A replacement withdraws the poll while a post fires it: the poll is either
/// answered or withdrawn, never both, and never neither.
#[test]
fn a_withdrawal_and_a_post_never_both_take_a_poll() {
    loom::model(|| {
        let poll = Arc::new(Once::new());

        let poster = {
            let poll = poll.clone();
            loom::thread::spawn(move || poll.fire())
        };
        let replacer = {
            let poll = poll.clone();
            loom::thread::spawn(move || poll.withdraw())
        };
        let answered = poster.join().unwrap();
        let withdrawn = replacer.join().unwrap();

        assert!(answered != withdrawn, "answered={answered} withdrawn={withdrawn}");
        assert!(!poll.armed());
    });
}
