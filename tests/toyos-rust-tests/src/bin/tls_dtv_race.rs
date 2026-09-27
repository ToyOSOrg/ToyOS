//! C1: no thread of a process may reach a new thread's TLS block before the
//! kernel has rebased the block's pointers to the address it maps it at.
//!
//! One worker at a time runs on one reused stack, so the only arena allocation
//! that churns is the worker's TLS block, placed at one address round after
//! round. A sibling probes that address with `random` — `BadAddress` while it
//! is unmapped — and once it is mapped stores 0 into its DTV slot 0, which a
//! rebase still to come turns into a `p - phys` underflow: a kernel panic
//! reached from userland.
//!
//! Every spawn after the first, which places the block, carries
//! `kernel/src/loader/tls.rs`'s `rebase_window::MARK`. A kernel armed with
//! `tls-rebase-window` holds such a spawn before its rebase until the sibling's
//! store lands if the block is already reachable, and says it is not if it is
//! not; `tls_rebase_window` runs this there and reads which. Unarmed, the
//! window is the scheduler's to hit.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};
use std::time::{Duration, Instant};
use toyos_abi::syscall;

/// `kernel/src/loader/tls.rs`'s `rebase_window::MARK`.
const MARK: u64 = 0x5eed_c0de_71b0_0001;
/// 2 MiB, the TLS block's alignment and (for this process's small TLS) its size.
const BLOCK: u64 = 2 * 1024 * 1024;
/// DTV slot 0 (module 1) sits two words into the block: a generation word, a
/// length word, then the entries. `kernel/src/loader/tls.rs` owns this layout.
const DTV_SLOT0: u64 = 16;
/// Spawn/retire rounds; every one after the first is watched.
const ROUNDS: u64 = 16;
/// A liveness bound on another thread's store, never a pace.
const BOUND: Duration = Duration::from_secs(10);

/// The block base the sibling stores into.
static TARGET: AtomicU64 = AtomicU64::new(0);
/// The current worker has published `TARGET`.
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
/// Rounds in which the sibling found the block mapped and stored into it.
static ENGAGED: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// A datum in this program's static TLS, which lies in the thread's block.
    static ANCHOR: u8 = const { 0 };
}

/// Spin until `done`, or panic naming `what` past [`BOUND`].
fn until(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + BOUND;
    while !done() {
        assert!(Instant::now() < deadline, "tls_dtv_race: {what} did not come within {BOUND:?}");
        core::hint::spin_loop();
    }
}

/// One worker: publish its block's base and wait to be retired. Runs on a
/// fixed stack; only one worker exists at a time.
extern "C" fn worker(_arg: u64) {
    let block = ANCHOR.with(|anchor| anchor as *const u8 as u64) & !(BLOCK - 1);
    TARGET.store(block, SeqCst);
    READY.store(true, SeqCst);
    until("leave to exit", || GO_EXIT.load(SeqCst));
    syscall::thread_exit(0);
}

/// Whether `v` is mapped, by the `random` syscall — it answers `BadAddress`
/// while the page is unmapped and writes into it otherwise. The probe writes one
/// byte at the block's generation word (harmless).
fn mapped(v: u64) -> bool {
    // SAFETY: on `BadAddress` nothing is touched; the caller stores only after
    // this returns true and only while the block stays mapped (see `sibling`).
    let probe = unsafe { core::slice::from_raw_parts_mut(v as *mut u8, 1) };
    syscall::random(probe).is_ok()
}

fn sibling() {
    while !STOP.load(SeqCst) {
        if PAUSE.load(SeqCst) {
            PAUSED_ACK.store(true, SeqCst);
            core::hint::spin_loop();
            continue;
        }
        PAUSED_ACK.store(false, SeqCst);
        let v = TARGET.load(SeqCst);
        // Wait for the block to be mapped (a spawn is placing it), then store 0
        // into DTV slot 0 in a tight loop with no syscall, until `PAUSE`.
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

fn main() {
    let sib = std::thread::Builder::new()
        .name("dtv-sibling".into())
        .spawn(sibling)
        .expect("spawn sibling");

    const STACK: usize = 256 * 1024;
    let stack = vec![0u8; STACK].leak();
    let base = stack.as_ptr() as u64;
    let top = (base + STACK as u64) & !15;
    let entry = (worker as *const ()).expose_provenance() as u64;

    for round in 0..ROUNDS {
        READY.store(false, SeqCst);
        GO_EXIT.store(false, SeqCst);
        PAUSE.store(false, SeqCst);
        let arg = if round == 0 { 0 } else { MARK };
        // SAFETY: `worker` is a valid entry; `top`/`base` describe the leaked stack.
        let tid = unsafe { syscall::thread_spawn(entry, top, arg, base) };
        assert!(syscall::SyscallError::from_u64(tid).is_none(), "thread_spawn failed: {tid}");
        until("the worker's start", || READY.load(SeqCst));
        until("the sibling's store into the block", || ENGAGED.load(SeqCst) > round);
        // Stood down and acknowledged before the join frees the block, so no
        // store is in flight against a free.
        PAUSE.store(true, SeqCst);
        until("the sibling standing down", || PAUSED_ACK.load(SeqCst));
        GO_EXIT.store(true, SeqCst);
        assert_eq!(syscall::thread_join(tid), 0, "round {round}: join");
    }

    STOP.store(true, SeqCst);
    sib.join().expect("join sibling");
    println!("tls_dtv_race: {ROUNDS} rounds, {} watched, the sibling stored into every one", ROUNDS - 1);
}
