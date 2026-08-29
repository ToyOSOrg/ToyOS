//! Loom: the panic path's poison bank.
//!
//! `scheduler::POISONED` was one slot per CPU and `poison_tid` swapped into it,
//! so a second death on one CPU before its next idle trip erased the first.
//! The negative case is a cargo feature rather than a comment:
//!
//! ```text
//! cargo test --manifest-path kernel-loom/Cargo.toml --features poison-overwrite \
//!   --test poison_set
//! ```
//!
//! restores the erasing swap, and [`a_second_death_banks_beside_the_first`] must red.

use kernel_loom::poison::{PoisonSet, SLOTS};
use loom::sync::Arc;
use loom::thread;

/// Two deaths on one CPU before its idle trip: the reaper must be handed both.
#[test]
fn a_second_death_banks_beside_the_first() {
    loom::model(|| {
        let bank = PoisonSet::new();
        assert!(bank.bank(1), "an empty bank refused the first death");
        assert!(bank.bank(2), "a bank with seven free slots refused the second");
        let mut got = Vec::new();
        bank.drain(|id| got.push(id));
        got.sort_unstable();
        assert_eq!(got, [1, 2], "a banked death was erased");
    });
}

/// A death landing while another CPU drains is handed to that drain or the next — exactly once.
#[test]
fn a_death_racing_a_drain_is_never_lost() {
    loom::model(|| {
        let bank = Arc::new(PoisonSet::new());
        assert!(bank.bank(1));
        let pusher = {
            let bank = Arc::clone(&bank);
            thread::spawn(move || assert!(bank.bank(2), "a racing death was refused"))
        };
        let mut handed = Vec::new();
        bank.drain(|id| handed.push(id));
        pusher.join().unwrap();
        bank.drain(|id| handed.push(id));
        handed.sort_unstable();
        assert_eq!(handed, [1, 2], "a death was lost or handed twice: {handed:?}");
    });
}

/// A full bank refuses the overflow and keeps everything it holds.
#[test]
fn past_capacity_the_bank_refuses_loudly() {
    loom::model(|| {
        let bank = PoisonSet::new();
        for i in 0..SLOTS as u64 {
            assert!(bank.bank(i + 1), "slot {i} refused before the bank was full");
        }
        assert!(!bank.bank(99), "a full bank claimed to accept a ninth death");
        let mut got = Vec::new();
        bank.drain(|id| got.push(id));
        got.sort_unstable();
        let want: Vec<u64> = (1..=SLOTS as u64).collect();
        assert_eq!(got, want, "the refused death displaced a banked one");
    });
}
