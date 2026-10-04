//! Loom: a counters read that finds a CPU's round answered copies out that
//! CPU's answer to it, and not the one before.
//!
//! A CPU answers a round by publishing its block (`seqlock::Published`) inside
//! `Shootdown::serve`'s closure, and the reader waits for `served` before it
//! snapshots the block. The seqlock keeps a snapshot whole; the edge from
//! `serve`'s release to `served`'s acquire is what makes it this round's. On
//! x86 every load is an acquire, so no guest test can fail here, and the
//! negative control is a feature rather than a comment:
//!
//! ```text
//! cargo test --manifest-path kernel/loom/Cargo.toml --features shootdown-served-relaxed \
//!   --test counters_round
//! ```
//!
//! makes `served` read `Relaxed`, and this file must red: the reader finds the
//! round answered and copies out something other than the answer.
#![cfg(feature = "loom")]

use std::sync::atomic::{AtomicBool, Ordering::SeqCst};

use kernel_loom::seqlock::Published;
use kernel_loom::shootdown::Shootdown;
use loom::sync::Arc;

/// What the CPU's block held before the round, and what it answers the round with.
const BEFORE: [u64; 2] = [1, 1];
const ANSWER: [u64; 2] = [2, 2];

/// How many times the reader looks for the answer.
const POLLS: usize = 3;

/// Set by any execution in which the reader found the round answered, outside
/// the model so it speaks for all of them: without it the bounded poll could
/// pass every execution by never reaching the assertion.
static READ: AtomicBool = AtomicBool::new(false);

#[test]
fn a_cpu_read_as_answered_is_read_with_its_answer() {
    READ.store(false, SeqCst);
    let mut builder = loom::model::Builder::new();
    builder.preemption_bound = Some(2);
    builder.check(|| {
        let rounds = Arc::new(Shootdown::new());
        let block = Arc::new(Published::new());
        block.publish(BEFORE);
        let generation = rounds.issue();

        let cpu1 = {
            let (rounds, block) = (rounds.clone(), block.clone());
            loom::thread::spawn(move || rounds.serve(1, || block.publish(ANSWER)))
        };

        for _ in 0..POLLS {
            if rounds.served(1, generation) {
                READ.store(true, SeqCst);
                assert_eq!(
                    block.snapshot(),
                    Some(ANSWER),
                    "cpu 1 read as having answered the round, and its block did not copy \
                     out that answer",
                );
                break;
            }
            loom::thread::yield_now();
        }
        cpu1.join().unwrap();
    });
    assert!(READ.load(SeqCst), "no interleaving found the round answered, so the assertion never ran");
}
