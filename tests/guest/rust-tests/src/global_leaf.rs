//! `SYS_DEBUG`'s `GLOBAL_LEAF_REWRITE`, for the two metal rows that judge how a
//! global kernel translation is dropped, and the control both run first.
//!
//! The action maps a global 2 MiB kernel leaf, reads through it, points it at a
//! second frame, invalidates as asked and reads again, on one CPU with
//! interrupts closed. **Metal rows and not guest tests**: a metal row is the
//! cheaper tier, and this host's TCG guest drops every translation on a `CR3`
//! write and has no `INVPCID`, so both wrong machine flushes read fresh there.

use toyos_abi::syscall::{self, debug_action as da};

/// The frame the second read saw after `how`, once this CPU has shown it reads
/// the first one with nothing invalidated: a CPU that read the second frame
/// then held no translation to go stale, and its fresh read after an
/// invalidation would be no evidence of that invalidation.
pub fn rewritten(how: u64) -> u64 {
    let withheld = syscall::debug_with(da::GLOBAL_LEAF_REWRITE, da::GLOBAL_LEAF_WITHHELD);
    assert_eq!(
        withheld,
        da::GLOBAL_LEAF_SAW_OLD,
        "the control: with nothing invalidated the second read did not see the first frame, so \
         this CPU held no translation to go stale and the row judges nothing",
    );
    syscall::debug_with(da::GLOBAL_LEAF_REWRITE, how)
}
