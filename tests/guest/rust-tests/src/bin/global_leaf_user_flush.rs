//! A global kernel leaf rewritten under the machine-wide shootdown a user-half
//! change asks is still read stale by the CPU that issued it: that flush is
//! `INVPCID` type 3 or a `CR3` reload, which keep global translations, and only
//! a kernel-half change asks the flush that drops them.

#[path = "../global_leaf.rs"]
mod global_leaf;

use toyos_abi::syscall::debug_action as da;

fn main() {
    let saw = global_leaf::rewritten(da::GLOBAL_LEAF_USER_SHOT_DOWN);
    assert_eq!(saw, da::GLOBAL_LEAF_SAW_OLD, "the user-half shootdown dropped a global kernel translation");
    println!("global_leaf_user_flush: the user-half shootdown kept the global translation");
}
