//! A global kernel leaf rewritten under a machine-wide shootdown is read fresh
//! by the CPU that issued it: the flush every CPU of the shootdown runs drops
//! global translations, which `INVPCID` type 3 and a `CR3` write both leave.

#[path = "../global_leaf.rs"]
mod global_leaf;

use toyos_abi::syscall::debug_action as da;

fn main() {
    let saw = global_leaf::rewritten(da::GLOBAL_LEAF_SHOT_DOWN);
    assert_eq!(saw, da::GLOBAL_LEAF_SAW_NEW, "the shootdown's flush left the old frame's global translation");
    println!("global_leaf_flush: the shootdown's flush dropped the global translation");
}
