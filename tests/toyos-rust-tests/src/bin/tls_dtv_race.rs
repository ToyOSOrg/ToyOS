//! C1: a sibling thread must not be able to corrupt the next thread's TLS
//! block between the kernel mapping it and the kernel finishing its rebase.
//!
//! At head the kernel rebases the block *before* it maps it, so a sibling never
//! sees a pointer the kernel has yet to fix. With the review's mutation (`fix`
//! moved after `map_range` and `drop(space)`), the block is visible while its
//! DTV still holds physical addresses, and a sibling that stores 0 into a DTV
//! slot makes the rebase's `entry - phys` underflow — a kernel panic reached
//! from userland, which is what this test denies.
//!
//! The race is arranged so it needs no luck to be *safe*: every store the
//! sibling makes is gated by a `random` probe that answers `BadAddress` while
//! the address is unmapped, and the block it targets is either a live worker's
//! (mapped for that worker's whole life) or, during the window under test, one
//! the kernel is mapping. The worker's block is reused at one virtual address
//! round after round — one worker at a time on a fixed stack, so the only
//! churning arena allocation is the kernel's TLS block — so the sibling can
//! hammer that address in advance of each spawn.
//!
//! Kernel liveness is the verdict: under the mutation the machine panics and
//! the guest dies; at head every round completes, every worker finds its own
//! TP and DTV self-consistent, and the heap still walks.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};
use toyos_abi::syscall;

/// 2 MiB, the TLS block's alignment and (for this process's small TLS) its size.
const BLOCK: u64 = 2 * 1024 * 1024;
const MASK: u64 = BLOCK - 1;
/// DTV slot 0 (module 1) sits two words into the block: a generation word, a
/// length word, then the entries. `kernel/src/loader/tls.rs` owns this layout.
const DTV_SLOT0: u64 = 16;

/// The block base the sibling hammers, or 0 while it must stand down.
static TARGET: AtomicU64 = AtomicU64::new(0);
/// The current worker has read its TP and published `TARGET`.
static READY: AtomicBool = AtomicBool::new(false);
/// The current worker may exit.
static GO_EXIT: AtomicBool = AtomicBool::new(false);
/// The sibling must not touch memory (a worker is being torn down).
static PAUSE: AtomicBool = AtomicBool::new(true);
/// The sibling has observed `PAUSE` and is idle at the top of its loop, past any
/// store — so the main thread may free the block without racing a store.
static PAUSED_ACK: AtomicBool = AtomicBool::new(false);
/// The sibling must end.
static STOP: AtomicBool = AtomicBool::new(false);
/// A worker found its TP and DTV inconsistent — a corruption the kernel let
/// through rather than panicking on.
static INCONSISTENT: AtomicBool = AtomicBool::new(false);
/// How many times the sibling found the block mapped and hammered it — proof it
/// engaged a real reused block rather than spinning on an unmapped address.
static ENGAGED: AtomicU64 = AtomicU64::new(0);

/// The self-pointer the psABI puts at the thread pointer (`%fs:0` on variant
/// II); the block base is that rounded down to the allocation's alignment.
#[cfg(target_arch = "x86_64")]
fn fs_base() -> u64 {
    let tp: u64;
    // SAFETY: reads the thread-pointer self-word the kernel wrote at TP+0.
    unsafe { core::arch::asm!("mov {}, fs:0", out(reg) tp, options(nostack, readonly)) };
    tp
}

/// One worker: publish its block, prove its own TP/DTV consistent, and wait to
/// be retired. Runs on a fixed stack; only one worker exists at a time.
extern "C" fn worker(_arg: u64) {
    let tp = fs_base();
    let v = tp & !MASK;
    // TP+8 is the DTV pointer, which the kernel rebased to the block base. A
    // block the kernel published half-rebased would fail this.
    // SAFETY: TP+8 is inside this thread's own TCB.
    let dtv = unsafe { ((tp + 8) as *const u64).read_volatile() };
    if dtv != v {
        INCONSISTENT.store(true, SeqCst);
    }
    TARGET.store(v, SeqCst);
    READY.store(true, SeqCst);
    while !GO_EXIT.load(SeqCst) {
        core::hint::spin_loop();
    }
    syscall::thread_exit(0);
}

/// Whether `addr` is mapped, by the `random` syscall — it answers `BadAddress`
/// while the page is unmapped and writes into it otherwise. The probe writes one
/// byte at the block's generation word (harmless).
fn mapped(v: u64) -> bool {
    // A `&mut [u8]` over memory this thread does not own is the price of using
    // the mandated mapped-ness probe; nothing here reads it, and the syscall
    // checks the mapping before any access.
    // SAFETY: on `BadAddress` nothing is touched; the caller stores only after
    // this returns true and only while the block stays mapped (see `sibling`).
    let probe = unsafe { core::slice::from_raw_parts_mut(v as *mut u8, 1) };
    syscall::random(probe).is_ok()
}

fn sibling() {
    while !STOP.load(SeqCst) {
        if PAUSE.load(SeqCst) {
            // Idle and past any store: the main thread waits for this before it
            // frees the block, so no store is ever in flight against a free.
            PAUSED_ACK.store(true, SeqCst);
            core::hint::spin_loop();
            continue;
        }
        PAUSED_ACK.store(false, SeqCst);
        let v = TARGET.load(SeqCst);
        // Wait for the block to be mapped (a spawn is placing it), then hammer 0
        // into DTV slot 0 to arm the rebase underflow, in a tight loop with no
        // syscall so the store lands inside the map-to-rebase window. `PAUSE`
        // ends the loop before the main thread frees the block.
        if v != 0 && mapped(v) {
            ENGAGED.fetch_add(1, SeqCst);
            while !PAUSE.load(SeqCst) && !STOP.load(SeqCst) {
                // SAFETY: confirmed mapped; the worker holding it does not exit
                // until the main thread pauses this loop and waits for the ack.
                unsafe { ((v + DTV_SLOT0) as *mut u64).write_volatile(0) };
            }
        }
    }
}

/// How many spawn/retire rounds to run; each is one race attempt, the sibling
/// hammering the block's DTV slot 0 through the whole map-to-rebase window, and
/// the block reused at one address across all of them. Bounded so the run
/// finishes on the dev host's TCG, where two busy vCPUs and a kernel log per
/// spawn make each round dear; under the mutation the panic comes in the first
/// rounds.
const ROUNDS: u64 = 800;

fn main() {
    if cfg!(not(target_arch = "x86_64")) {
        println!("tls_dtv_race: only x86_64 reads %fs:0; skipping");
        return;
    }

    let sib = std::thread::Builder::new()
        .name("dtv-sibling".into())
        .spawn(sibling)
        .expect("spawn sibling");

    // One reused stack: with a single worker alive at a time, the only arena
    // allocation that churns is the kernel's per-thread TLS block, so it is
    // reused at one virtual address — the one the sibling hammers.
    const STACK: usize = 256 * 1024;
    let stack = vec![0u8; STACK].leak();
    let base = stack.as_ptr() as u64;
    let top = (base + STACK as u64) & !15;

    let mut rounds = 0u64;
    for _ in 0..ROUNDS {
        READY.store(false, SeqCst);
        GO_EXIT.store(false, SeqCst);
        // Let the sibling hammer the address a retiring block last held; the
        // fresh spawn maps that same address, and the window under test is
        // between that map and the rebase.
        PAUSE.store(false, SeqCst);
        // SAFETY: `worker` is a valid entry; `top`/`base` describe the leaked stack.
        let entry = (worker as *const ()).expose_provenance() as u64;
        let tid = unsafe { syscall::thread_spawn(entry, top, 0, base) };
        assert!(syscall::SyscallError::from_u64(tid).is_none(), "thread_spawn failed: {tid}");
        while !READY.load(SeqCst) {
            core::hint::spin_loop();
        }
        // Retire the worker with the sibling stood down, so no store can land in
        // a block the kernel is freeing. Wait for the sibling to acknowledge it
        // is idle — a store already in flight completes while the block is still
        // mapped, before the join frees it.
        PAUSE.store(true, SeqCst);
        while !PAUSED_ACK.load(SeqCst) {
            core::hint::spin_loop();
        }
        GO_EXIT.store(true, SeqCst);
        syscall::thread_join(tid);
        rounds += 1;

        if rounds % 4096 == 0 {
            // The heap still walks: allocate, touch, free.
            let probe = vec![rounds as u8; 4096];
            assert_eq!(probe[0], rounds as u8);
        }
    }

    STOP.store(true, SeqCst);
    sib.join().expect("join sibling");

    assert!(!INCONSISTENT.load(SeqCst), "a worker's TP and DTV disagreed — the block was corrupted");

    // The kernel is still alive: a plain thread spawns, runs and joins.
    let n = std::thread::spawn(|| 0xC1u64).join().expect("final thread");
    assert_eq!(n, 0xC1);

    println!(
        "tls_dtv_race: {rounds} rounds, {} engaged, kernel alive, TP/DTV consistent",
        ENGAGED.load(SeqCst)
    );
}
