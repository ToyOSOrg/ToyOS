//! Loom: `OwedLock`'s handoff.
//!
//! A CPU that `try_lock_or_owe` turns away halts on the work it left, so it
//! must come back: either its retry took the lock, or the holder's release saw
//! its bit and answered it. That is a store-buffer pair, an owing store then a
//! read of `now` against the release's store to `now` then a read of the owed
//! word, and x86's TSO lets either read miss the other side's store, so no
//! guest test can stand in for it.
//!
//! ```text
//! cargo test --manifest-path kernel/loom/Cargo.toml --features owed-fence-off \
//!   --test owed_lock
//! ```
//!
//! drops the fence from both sides and this file must red.

#![cfg(feature = "loom")]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use kernel_loom::sync::OwedLock;
use loom::sync::Arc;

/// The CPU the model turns away.
const TURNED: u32 = 3;

/// What the releases answered in this execution; loom runs executions one at a
/// time on one OS thread, so a `std` static reset per execution is the record.
static ANSWERED: AtomicU64 = AtomicU64::new(0);

fn record(owed: u64) {
    ANSWERED.fetch_or(owed, Ordering::Relaxed);
}

#[test]
fn a_turned_away_cpu_holds_the_lock_or_is_answered() {
    static WON: AtomicBool = AtomicBool::new(false);
    static TURNED_AWAY: AtomicBool = AtomicBool::new(false);

    loom::model(|| {
        ANSWERED.store(0, Ordering::Relaxed);
        let lock = Arc::new(OwedLock::new(0u32, record));
        let held = lock.try_lock().expect("an untaken lock is taken");

        let other = {
            let lock = lock.clone();
            loom::thread::spawn(move || lock.try_lock_or_owe(TURNED).map(drop).is_some())
        };
        drop(held);
        let won = other.join().unwrap();

        let answered = ANSWERED.load(Ordering::Relaxed) & (1 << TURNED) != 0;
        assert!(won || answered, "cpu {TURNED} was turned away and nobody answered it");
        if won {
            WON.store(true, Ordering::Relaxed);
        } else {
            TURNED_AWAY.store(true, Ordering::Relaxed);
        }
    });

    assert!(
        WON.load(Ordering::Relaxed) && TURNED_AWAY.load(Ordering::Relaxed),
        "the model never both turned the cpu away and let it in, so one side of the handoff went unchecked",
    );
}
