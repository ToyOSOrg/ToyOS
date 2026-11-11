//! Under `CR4.FSGSBASE` a Ring 3 thread writes its own FS and GS bases, and
//! the switch keeps each thread's: two threads pinned to one CPU each write
//! bases of their own, yield, and read them back while the other writes
//! others. One thread's GS base is a kernel-half address, which `WRGSBASE`
//! allows and no kernel entry may take for its own.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Barrier;
use std::thread;

use core_affinity::CoreId;
use toyos_abi::syscall::SYS_YIELD;

#[path = "../arch/fsgsbase.rs"]
mod fsgsbase;

const ROUNDS: u64 = 500;

/// Each thread's finished rounds: a change in the other's across one of a
/// thread's rounds is the other having run on their one CPU meanwhile.
static ROUNDS_DONE: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];

/// Both threads pinned before either starts its rounds, so each runs across
/// the other's.
static PINNED: Barrier = Barrier::new(2);

fn main() {
    let sibling = thread::spawn(|| rounds(1));
    let mine = rounds(0);
    let theirs = sibling.join().expect("the sibling's rounds");
    assert!(mine > 0 && theirs > 0, "the two threads never ran across each other's rounds: {mine} and {theirs}");
    println!(
        "fsgs_bases_per_thread: two threads on cpu0 read back their own FS and GS bases over {ROUNDS} yields each, \
         the other running across {mine} and {theirs} of them"
    );
}

/// Thread `me`'s rounds, and how many of them the other thread ran across.
fn rounds(me: usize) -> u64 {
    assert!(core_affinity::set_for_current(CoreId { id: 0 }), "pin thread {me} to cpu0");
    PINNED.wait();
    let mut crossed = 0;
    for round in 0..ROUNDS {
        let fs = 0x0000_7000_0000_0000 | (me as u64) << 40 | round << 4;
        let gs = [0x0000_6000_0000_0000, 0xffff_9000_0000_0000][me] | round << 4;
        let other = ROUNDS_DONE[1 - me].load(Ordering::Acquire);
        let read = fsgsbase::bases_across_a_yield(fs, gs, SYS_YIELD);
        assert_eq!(read, (fs, gs), "thread {me} round {round} wrote FS {fs:#x} and GS {gs:#x} and read {read:#x?}");
        ROUNDS_DONE[me].fetch_add(1, Ordering::Release);
        if ROUNDS_DONE[1 - me].load(Ordering::Acquire) != other {
            crossed += 1;
        }
    }
    crossed
}
