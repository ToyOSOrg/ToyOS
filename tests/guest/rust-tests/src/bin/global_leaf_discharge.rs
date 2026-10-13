//! A global kernel leaf rewritten through its own discharge is read fresh by
//! the CPU that wrote it: the discharge dropped the global translation, which
//! `INVPCID` type 0 and a `CR3` write both leave.

#[path = "../global_leaf.rs"]
mod global_leaf;

use toyos_abi::syscall::debug_action as da;

fn main() {
    let saw = global_leaf::rewritten(da::GLOBAL_LEAF_DISCHARGED);
    assert_eq!(saw, da::GLOBAL_LEAF_SAW_NEW, "the write's discharge left the old frame's global translation");
    println!("global_leaf_discharge: the discharge dropped the global translation");
}
