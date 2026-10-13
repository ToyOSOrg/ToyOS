//! `SYS_DEBUG`'s `GLOBAL_LEAF_REWRITE`, for the three metal rows that judge
//! which flush drops a global kernel translation and which keeps it.
//!
//! The action maps a global 2 MiB kernel leaf, reads through it, points it at a
//! second frame, reads again with nothing invalidated (the control),
//! invalidates as asked and reads a third time, on one CPU with interrupts
//! closed. **Metal rows and not guest tests**: a metal row is the cheaper tier,
//! and this host's TCG guest drops every translation on a `CR3` write and has
//! no `INVPCID`, so it reads fresh where a CPU that keeps a global translation
//! reads stale.

use toyos_abi::syscall::{self, debug_action as da};

/// The frame the third read saw after `how`, refused where what it read after
/// `how` would be no evidence of `how`: the control saw the second frame, so
/// that CPU held no translation to go stale; or a user-half shootdown answered
/// wide, covering a kernel-half one another CPU issued inside it.
pub fn rewritten(how: u64) -> u64 {
    let saw = syscall::debug_with(da::GLOBAL_LEAF_REWRITE, how);
    assert_ne!(
        saw,
        da::GLOBAL_LEAF_UNHELD,
        "the control: with nothing invalidated the second read did not see the first frame, so \
         this CPU held no translation to go stale and the row judges nothing",
    );
    assert_ne!(
        saw,
        da::GLOBAL_LEAF_WIDENED,
        "the user-half shootdown's answer covered another CPU's kernel-half one, which drops \
         global translations, so the row judges nothing",
    );
    saw
}
