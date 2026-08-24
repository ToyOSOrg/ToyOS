//! Two threads faulting on one 2 MiB window get one page between them.
//!
//! The demand pager cannot hold the address space across a fill — a fill is a
//! 2 MiB zeroing or up to 512 device reads — so it asks "is this window
//! mapped?", unlocks, fills, and comes back. Both threads of a pair get "no" to
//! that first question. What decides is the second one, inside the critical
//! section that writes the entry (`AddressSpace::map_window_if_absent`).
//!
//! **What the harm looks like from here, and why it is detected rather than
//! inferred.** When both installs went through, the PDE ended up naming the
//! second frame while the first thread's CPU still held a translation to the
//! first — a fault issues no shootdown, and what `write_pde` derives reaches
//! only the CPU that wrote it. So each thread writes its own marker into
//! whichever frame it is on, and then every thread reads every marker: a thread
//! left on the orphaned frame cannot see the other's marker, and a thread whose
//! translation was replaced cannot see its own, because the write it made
//! before the second install went to the frame that is now unreachable. Either
//! way a slot reads zero, which is what the frame was filled with and what no
//! marker ever is. A window that really was filled twice is caught; a window
//! only one thread ever faulted on is not raced and has nothing to catch.
//!
//! **`solo` is why the numbers mean anything.** It is this same binary, same
//! thread count, same statics, same windows — the one difference is that each
//! window is touched by one thread instead of by all of them, so no two faults
//! ever meet. A thread cannot lose a race to itself, so `solo` keeps one page
//! per window and `race` must keep no more: that is the correctness assertion,
//! and it is the one a kernel that installs both fills fails. `race` filling
//! *more windows* than `solo` while keeping the same number is the discarded
//! fills, and that is the evidence the test still stages the race it was
//! written for — without it a scheduling change could stop the threads
//! overlapping and leave every assertion here passing on a run that raced
//! nothing. Whether they overlap is the host's to decide and not the guest's,
//! so it is retried rather than asserted once: see [`RACE_ATTEMPTS`].
//!
//! The counters come from `SYS_PROCESS_STATS`, which is a question about a
//! process object rather than about the caller — so the racing is done by a
//! child and read afterwards by its parent. `fault_demand_count` and
//! `fault_zero_count` count *fills*, nothing else touches them, and their sum
//! is what a fill costs whether or not it was kept. `alloc_count` is what the
//! kernel kept plus this process's mappings and dynamic TLS blocks; that second
//! term is not separable from here, which is why every claim below is about the
//! *difference* between two children that make identical calls.

use std::cell::UnsafeCell;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;

use std::os::toyos::process::ChildExt;
use toyos_abi::syscall::{self, ProcessStats};

const PAGE_2M: usize = 2 * 1024 * 1024;

/// How many 2 MiB windows each child races over.
///
/// Sixty-four because the evidence wanted from a run is a *rate*: a single
/// window that happened not to overlap says nothing either way, and sixty-four
/// of them is 128 MiB of a 4 GiB guest even when every one of them is filled
/// twice. It is also what keeps the `solo`/`race` fill difference far larger
/// than any process-to-process difference in how the two children faulted their
/// own images in.
const WINDOWS: usize = 64;

/// **The windows are this program's own `.bss`, because that is where demand
/// paging still happens.** `SYS_MMAP` allocates and maps its whole range up
/// front — an anonymous mapping is a `RegionKind::Mapped` and the fault path
/// refuses to fill one on purpose — and so does the stack. What is left
/// demand-paged is the image: the file-backed part of each `PT_LOAD` and the
/// zero tail past its `filesz`, which is this array. It costs nothing in the
/// binary, since `.bss` has no file bytes.
///
/// One window more than is raced, so that a 2 MiB-aligned start can be taken
/// inside the array without asking the linker for an alignment it has no reason
/// to give: every window from that start lies wholly within these bytes, and
/// the partial window before it — which may share a frame with other statics
/// and so be mapped already — is skipped.
const PLAYGROUND_BYTES: usize = (WINDOWS + 1) * PAGE_2M;

struct Playground(UnsafeCell<[u8; PLAYGROUND_BYTES]>);

// SAFETY: nothing dereferences this as a Rust reference. Every access is a
// `read_volatile`/`write_volatile` through the raw pointer `UnsafeCell::get`
// answers, and which threads touch which bytes is decided by `worker` — thread
// `t` owns byte range `t * 4096 .. t * 4096 + 8` of each window and no other
// thread writes it.
unsafe impl Sync for Playground {}

static PLAYGROUND: Playground = Playground(UnsafeCell::new([0u8; PLAYGROUND_BYTES]));

/// At most this many divergent slots are described before the rest are only
/// counted. A failure here is per window and per thread, so an unbounded report
/// on a fully broken kernel is a thousand lines of the same sentence.
const MAX_REPORTED: usize = 8;

/// Non-zero for every `(window, thread)`, and different for every one of them.
///
/// Zero is what the kernel filled the frame with, so it is the one value that
/// must never be a marker: a slot reading zero is a slot nobody's write
/// reached. Carrying the window means a stale translation into a *different*
/// window cannot answer this window's question by accident.
fn marker(w: usize, t: usize) -> u64 {
    0x00D0_0000_0000_0000 | ((w as u64) << 16) | (t as u64 + 1)
}

/// A barrier that spins rather than parks.
///
/// The two threads have to reach the same cold window close enough together
/// that both are inside the fault path at once, and a futex wake is the wrong
/// order of magnitude to release them with — it puts milliseconds of skew into
/// exactly the measurement this file exists to make. The guest is two vCPUs and
/// two threads, so the spin is on a CPU nothing else wants; the kernel preempts
/// regardless, so a wider guest makes this slow and never stuck.
struct SpinBarrier {
    n: usize,
    waiting: AtomicUsize,
    generation: AtomicUsize,
}

impl SpinBarrier {
    fn new(n: usize) -> Self {
        Self { n, waiting: AtomicUsize::new(0), generation: AtomicUsize::new(0) }
    }

    fn wait(&self) {
        let gen = self.generation.load(Ordering::Relaxed);
        if self.waiting.fetch_add(1, Ordering::AcqRel) + 1 == self.n {
            // Nothing can start the next round until the generation moves, so
            // the reset is unobservable before the store below publishes it.
            self.waiting.store(0, Ordering::Relaxed);
            self.generation.fetch_add(1, Ordering::Release);
        } else {
            while self.generation.load(Ordering::Acquire) == gen {
                std::hint::spin_loop();
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Every thread touches every window. The windows are cold, so every thread
    /// faults on every window and the pair of them is the race.
    Race,
    /// One thread touches each window, round-robin. Same threads, same statics,
    /// same windows, no two faults on one window.
    Solo,
}

impl Mode {
    fn parse(arg: &str) -> Option<Self> {
        match arg {
            "race" => Some(Mode::Race),
            "solo" => Some(Mode::Solo),
            _ => None,
        }
    }
}

/// The racing half: `WINDOWS` rounds, each one a barrier, a touch and a check.
///
/// `base` is a `usize` and not a pointer because it crosses into the threads,
/// and every access below is a `read_volatile`/`write_volatile` on an address
/// derived from it — what the address names is the kernel's to change under
/// this thread's feet, which is not a thing a `&mut [u64]` may describe.
fn worker(mode: Mode, base: usize, t: usize, threads: usize, barrier: &SpinBarrier, bad: &AtomicUsize) {
    for w in 0..WINDOWS {
        let touches = mode == Mode::Race || w % threads == t;

        // Released together, so both threads reach a window nothing has mapped
        // within a few hundred nanoseconds of each other and the second one's
        // "is it mapped?" is answered while the first one is still filling.
        barrier.wait();
        if touches {
            let slot = (base + w * PAGE_2M + t * 4096) as *mut u64;
            // SAFETY: `base` is the first 2 MiB boundary inside `PLAYGROUND`,
            // which is `(WINDOWS + 1) * PAGE_2M` bytes, so `WINDOWS` whole
            // windows from there are inside it; `w < WINDOWS` and
            // `t * 4096 + 8 <= PAGE_2M` is asserted before any thread starts.
            // No other thread writes this thread's eight bytes.
            unsafe { slot.write_volatile(marker(w, t)) };
        }

        // Every write to this window is in the past on the other side of this,
        // whichever frame it landed in.
        barrier.wait();

        // Every thread's marker in race mode, because either side of a
        // divergence is a finding: a thread left on the orphaned frame misses
        // the other's marker, one whose translation was replaced misses its
        // own. In solo mode there is one marker in this window and one thread
        // that wrote it.
        //
        // A range and not a collection: nothing in this loop may allocate, or
        // the two modes would reach the allocator a different number of times
        // and `alloc_count` would carry the difference.
        let checked = match mode {
            Mode::Race => 0..threads,
            Mode::Solo if touches => t..t + 1,
            Mode::Solo => 0..0,
        };
        for u in checked {
            let slot = (base + w * PAGE_2M + u * 4096) as *mut u64;
            // SAFETY: as above.
            let got = unsafe { slot.read_volatile() };
            let want = marker(w, u);
            if got != want {
                let n = bad.fetch_add(1, Ordering::Relaxed);
                if n < MAX_REPORTED {
                    println!(
                        "  window {w}: thread {t} reads {got:#018x} where thread {u} wrote \
                         {want:#018x} — the two are on different frames, so this window was \
                         filled twice and one fill was installed over the other"
                    );
                }
            }
        }
    }
}

fn child(mode: Mode) {
    // `syscall::cpu_count` and not `std::thread::available_parallelism`, which
    // answers 1 on a two-CPU guest — a std gap this file is not the place to
    // fix, and a wrong answer here would silently unstage the race.
    //
    // One thread per CPU exactly. Fewer and the window is not contended; more
    // and a thread spinning at the barrier is holding the CPU another
    // participant needs, so every round costs a timeslice instead of a fault.
    let threads = syscall::cpu_count() as usize;
    assert!(
        threads >= 2,
        "this test needs a guest with more than one CPU: a second thread is the whole staging, \
         and on {threads} CPU(s) nothing here races anything"
    );

    // The first 2 MiB boundary at or after the array. Every window from here is
    // wholly inside it and has been touched by nothing.
    let base = (PLAYGROUND.0.get() as usize).next_multiple_of(PAGE_2M);
    assert!(
        threads * 4096 <= PAGE_2M,
        "{threads} threads want {} bytes of each 2 MiB window",
        threads * 4096
    );

    let barrier = Arc::new(SpinBarrier::new(threads));
    let bad = Arc::new(AtomicUsize::new(0));

    let handles: Vec<_> = (1..threads)
        .map(|t| {
            let barrier = Arc::clone(&barrier);
            let bad = Arc::clone(&bad);
            thread::spawn(move || worker(mode, base, t, threads, &barrier, &bad))
        })
        .collect();
    // Thread 0 is this one: a panic in a spawned thread leaves the others
    // spinning at a barrier that will never release, so nothing below asserts
    // until every round is over.
    worker(mode, base, 0, threads, &barrier, &bad);
    for h in handles {
        h.join().expect("a worker thread panicked");
    }

    let bad = bad.load(Ordering::Relaxed);
    assert_eq!(
        bad, 0,
        "{bad} slot(s) of {WINDOWS} windows read what no thread wrote there: two threads of \
         this process are on different frames for one address"
    );
    println!("  child({}): {WINDOWS} windows, {threads} threads, every marker readable by every \
              thread", if mode == Mode::Race { "race" } else { "solo" });
}

fn stats_of(child: &std::process::Child) -> ProcessStats {
    let mut stats = ProcessStats::default();
    syscall::process_stats(toyos_abi::RawHandle(child.as_raw_handle()), &mut stats)
        .expect("an exited child still answers its accounting");
    stats
}

/// The fills a process paid for, kept or not. Nothing but the demand pager
/// moves either counter, so this is the fault path's own work and does not
/// carry mappings or TLS blocks the way `alloc_count` does.
fn fills(s: &ProcessStats) -> u64 {
    u64::from(s.fault_demand_count) + u64::from(s.fault_zero_count)
}

/// Run one child to completion and take its accounting off its object.
///
/// The reading is printed before the child's exit status is judged, so a run
/// where the content check fired still says what the kernel did — the numbers
/// are the instrument, and a failing arm is exactly when they are wanted.
fn run(exe: &std::path::Path, arg: &str) -> ProcessStats {
    let mut child = Command::new(exe).arg(arg).spawn().unwrap_or_else(|e| panic!("spawn {arg}: {e}"));
    let status = child.wait().unwrap_or_else(|e| panic!("wait {arg}: {e}"));
    let stats = stats_of(&child);
    println!(
        "  {arg}: {} fills, {} kept, {} ns in faults ({} ns/fill)",
        fills(&stats),
        stats.alloc_count,
        stats.fault_ns,
        stats.fault_ns / fills(&stats).max(1),
    );
    assert!(status.success(), "the {arg} child exited with {status}");
    stats
}

/// How many race children are run before "the threads never overlapped" is a
/// failure rather than a retry.
///
/// **The staging is not guaranteed by construction and no guest can make it
/// so.** Whether the second thread reaches its check while the first is filling
/// is decided by whether the host is running the other vCPU just then, and a
/// guest has no lever on that. On an idle dev host every one of the sixty-four
/// windows raced on eight runs out of eight; under 28-way load on fourteen
/// cores, fifty to fifty-seven of them did. But one run in a batch staged
/// nothing at all, so the honest shape is to attempt it again rather than to
/// call a scheduling accident a kernel defect — or to drop the witness and let
/// this test go quietly vacuous. Four attempts, because each is a fraction of a
/// second and the correctness assertions run on every one of them.
const RACE_ATTEMPTS: usize = 4;

fn main() {
    if let Some(mode) = std::env::args().nth(1).as_deref().and_then(Mode::parse) {
        return child(mode);
    }

    let exe = std::env::current_exe().expect("current_exe");
    let solo = run(&exe, "solo");
    let solo_fills = fills(&solo);

    assert!(
        solo_fills >= WINDOWS as u64,
        "the solo child filled {solo_fills} windows, fewer than the {WINDOWS} it touched — it \
         did not do the work this test compares against"
    );

    let mut staged = None;
    for attempt in 1..=RACE_ATTEMPTS {
        let race = run(&exe, "race");

        // The correctness assertion, and the one a kernel that installs both
        // fills fails: the two children keep the same pages and make the same
        // mappings, so a race child holding more is a window it paid for twice.
        // It is asked of every attempt, staged or not.
        assert!(
            race.alloc_count <= solo.alloc_count,
            "the race child kept {} pages where the solo child kept {} — {} window(s) were \
             filled twice and both fills installed",
            race.alloc_count,
            solo.alloc_count,
            race.alloc_count - solo.alloc_count,
        );

        // The witness: a fill this child paid for and did not keep is two
        // threads having met inside one window.
        if fills(&race) > solo_fills {
            staged = Some((attempt, fills(&race) - solo_fills));
            break;
        }
        println!(
            "  attempt {attempt}: {} fills against the solo child's {solo_fills}, so no fill was \
             discarded and the two threads never overlapped",
            fills(&race)
        );
    }

    let Some((attempts, lost)) = staged else {
        panic!(
            "{RACE_ATTEMPTS} race children each filled exactly what the solo child did, so not \
             one of them ever had two threads inside one window: this test staged nothing and \
             its correctness assertions proved nothing"
        )
    };

    println!(
        "one window, one page: {lost} fill(s) lost the race and were dropped rather than \
         installed (attempt {attempts} of {RACE_ATTEMPTS})"
    );
}
