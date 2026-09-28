//! `SYS_FUTEX_WAKE` wakes **up to `count`** waiters on **this word**, and
//! answers how many — and a word whose frame is taken away ends the waits on
//! it rather than leaving them armed on somebody else's memory.
//!
//! Both halves of the first sentence are the ABI's own
//! (`toyos-abi/src/syscall.rs`: "wake up to `count` threads waiting on `addr`.
//! Returns number of threads woken"), and the completion cutover briefly
//! honoured neither. The count went to a 64-way bucket queue that nothing had
//! registered on since the park moved to the thread's own queue — so the return
//! was **provably always 0**, for every call in the machine — and the wake that
//! actually happened was an uncounted post to every waiter on the shared
//! bucket, which turns `pthread_cond_signal` into a broadcast and can spend one
//! thread's wake on a waiter of a different word.
//!
//! Nothing in the tree noticed, because nothing in the tree asks: `libc`'s
//! `pthread` discards the return, and the std fork's `RwLock::wake_writer`
//! reads a permanently-false answer as documented-but-pessimal. So this asks.
//!
//! **The two words are 256 bytes apart on purpose.** The bucket is
//! `(phys >> 2) % 64`, so words 256 bytes apart in one page land in the *same*
//! bucket by construction — which is the only way to test that a shared bucket
//! is not a shared wake. A page-aligned static is what makes their physical
//! offset equal their virtual one.
//!
//! **Four questions, and each one had to be given a schedule rather than
//! offered a race.**
//!
//! 1. [`counts`] — a count-limited wake names its word and answers how many.
//! 2. [`claim_semantics`] — a claim another call already took is neither
//!    counted nor charged against `limit`. Both are invisible unless the woken
//!    waiter is still on the bucket when the next call walks it, which is a
//!    state this test *arranges*: see the doc on that function.
//! 3. [`orphaned_by_unmap`] — a wait whose word is unmapped ends there. The
//!    token a futex waiter arms with is its word's **physical** address and
//!    nothing pins the frame, so without this the bucket keeps a node naming
//!    memory the PMM has already reissued: `mmap` hands the freshly freed frame
//!    to the next asker, and that process's `futex_wake` on the same offset
//!    wins the stale waiter's claim, is told 1 for a thread it did not wake,
//!    and leaves the real waiter parked for good. The sweeper below is another
//!    process asking exactly that question about frames this one just gave
//!    back.
//! 4. [`revoked_while_still_mapped`] — a wait that unmap ended stays ended
//!    when the waiter's own mapping of the word survives: another process's
//!    unmap of the same frame is the revoke, and the word still reads
//!    `expected`.

use std::io::Write;
use std::os::toyos::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::Duration;

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::syscap::SysCap;
use toyos_abi::syscall::{self, MmapFlags, MmapProt};
use toyos_abi::RawHandle;

#[path = "../roster.rs"]
mod roster;

/// Two futex words in one page, `FUTEX_BUCKETS * 4` bytes apart, so
/// `(phys >> 2) % 64` is the same for both.
#[repr(C, align(4096))]
struct SameBucket {
    word: AtomicU32,
    _pad: [u8; 256 - 4],
    sibling: AtomicU32,
}

static WORDS: SameBucket = SameBucket {
    word: AtomicU32::new(0),
    _pad: [0; 256 - 4],
    sibling: AtomicU32::new(0),
};

static WORD_RETURNED: AtomicU32 = AtomicU32::new(0);
static SIBLING_RETURNED: AtomicU32 = AtomicU32::new(0);
/// [`counts`]' waiters that have reached their `futex_wait`.
static COUNTS_ARMED: AtomicU32 = AtomicU32::new(0);

/// This binary's own path. The sweeper is another *process* asking about
/// frames this one freed, which is the whole of what the third arm is about,
/// and a second binary would be a second name in the shared registry for one
/// verdict.
const SELF: &str = "/system/bin/test_rs_futex_wake_counts";
/// `argv[1]` the sweeper is spawned with.
const SWEEP: &str = "sweep";
/// `argv[1]` of [`revoked_while_still_mapped`]'s child, and the slot it finds
/// the pipe in.
const LET_GO: &str = "let-go";
const INHERITED_PIPE: RawHandle = RawHandle(3);

/// How many 2 MiB frames the third arm parks a waiter on and then unmaps.
///
/// More than one because `sys_mmap` allocates the *lowest* free frame
/// (`pmm::alloc_contiguous` scans the bitmap from index 0), so any allocation
/// anywhere in the machine between the unmap and the sweeper's first `mmap`
/// takes one of these back. Four of them means an ordinary stray allocation
/// costs the arm one frame of reach rather than all of it.
const STALE_FRAMES: usize = 4;
/// How many fresh frames the sweeper asks about. Comfortably more than
/// [`STALE_FRAMES`], for the same reason.
const SWEEP_FRAMES: usize = 12;
const PAGE_2M: usize = 2 * 1024 * 1024;

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some(SWEEP) => return sweep(),
        Some(LET_GO) => return let_go(),
        _ => {}
    }
    let cap: SysCap = Endowments::get()
        .take(SYSCAP_LABEL)
        .expect("test-runner endows every binary it spawns a system capability");
    counts(&cap);
    claim_semantics(&cap);
    orphaned_by_unmap(&cap);
    revoked_while_still_mapped();
    timeout_is_its_own_answer();
    println!("futex_wake respects its count, names its word, says how many it woke, and ends");
}

/// The wait's own two answers, which the ABI names and the kernel could not
/// produce: `0` on a wake and `1` on a timeout.
///
/// **Both arms, because either one alone passes on a kernel that answers a
/// constant** — which is what this was: every return was 0, so a
/// `pthread_cond_timedwait` built on it could never report `ETIMEDOUT`, and the
/// honest answer and the wrong one were the same number.
///
/// The timeout is a real span and the wake is not raced against one: the woken
/// arm waits forever and is woken by this thread after the word changes, so no
/// margin decides anything. What the timed arm asserts is only that a wait
/// nobody wakes ends saying so — a slow host makes it later, never wrong.
fn timeout_is_its_own_answer() {
    static TIMED: AtomicU32 = AtomicU32::new(7);
    static WOKEN: AtomicU32 = AtomicU32::new(7);

    let timed_out = unsafe { syscall::futex_wait(TIMED.as_ptr(), 7, Some(50_000_000)) };
    assert_eq!(timed_out, 1, "a futex wait nobody woke answered {timed_out}, wanted the timeout");

    // …and the same call with the word already changed is the other answer,
    // which is what rules out a kernel that has started answering 1 to
    // everything.
    let changed = unsafe { syscall::futex_wait(TIMED.as_ptr(), 9, Some(50_000_000)) };
    assert_eq!(changed, 0, "a futex wait whose word did not match answered {changed}");

    let waiter = thread::spawn(|| unsafe { syscall::futex_wait(WOKEN.as_ptr(), 7, None) });
    wait_until_parked(&WOKEN, 1);
    WOKEN.store(8, Ordering::SeqCst);
    unsafe { syscall::futex_wake(WOKEN.as_ptr(), 1) };
    let woken = waiter.join().expect("the woken waiter panicked");
    assert_eq!(woken, 0, "a futex wait that was woken answered {woken}, wanted the wake");
}

/// A count-limited wake names its word and answers how many it woke.
///
/// **Parked by the kernel's roster, not by a probe**: a probe is a wake, and
/// the waiters it claims are still on their way back to the word when the
/// first wake below looks for them.
fn counts(cap: &SysCap) {
    let waiters: Vec<_> = (0..2)
        .map(|_| {
            thread::spawn(|| {
                COUNTS_ARMED.fetch_add(1, Ordering::SeqCst);
                // Returns only once the word has actually changed: the kernel's
                // `futex_wait` re-reads it after every wake, which is what makes
                // "was this thread told" observable at all.
                unsafe { syscall::futex_wait(WORDS.word.as_ptr(), 0, None) };
                WORD_RETURNED.fetch_add(1, Ordering::SeqCst);
            })
        })
        .collect();
    let sibling = thread::spawn(|| {
        COUNTS_ARMED.fetch_add(1, Ordering::SeqCst);
        unsafe { syscall::futex_wait(WORDS.sibling.as_ptr(), 0, None) };
        SIBLING_RETURNED.fetch_add(1, Ordering::SeqCst);
    });
    await_armed_and_blocked(cap, &COUNTS_ARMED, 3);

    // The word changes first, so a waiter that is told goes home instead of
    // re-parking — otherwise it would re-arm and be counted twice.
    WORDS.word.store(1, Ordering::SeqCst);

    let one = unsafe { syscall::futex_wake(WORDS.word.as_ptr(), 1) };
    assert_eq!(
        one, 1,
        "futex_wake(count=1) with two waiters answered {one}, and the ABI's answer is \
         the number of threads woken",
    );
    // No deadline: the woken waiter's return is the event, and one that never
    // comes is a hang the harness ceiling reds.
    while WORD_RETURNED.load(Ordering::SeqCst) == 0 {
        thread::yield_now();
    }
    let returned = WORD_RETURNED.load(Ordering::SeqCst);
    assert_eq!(
        returned, 1,
        "futex_wake(count=1) woke {returned} of two waiters — a count-limited wake is what \
         makes pthread_cond_signal a signal rather than a broadcast",
    );
    let leaked = SIBLING_RETURNED.load(Ordering::SeqCst);
    assert_eq!(
        leaked, 0,
        "waking one word woke a waiter of the other word in the same bucket — a shared \
         bucket is a place to arm, not a set of threads to wake",
    );

    let rest = unsafe { syscall::futex_wake(WORDS.word.as_ptr(), 10) };
    assert_eq!(rest, 1, "one waiter was left on this word, and futex_wake answered {rest}");

    WORDS.sibling.store(1, Ordering::SeqCst);
    let other = unsafe { syscall::futex_wake(WORDS.sibling.as_ptr(), 10) };
    assert_eq!(
        other, 1,
        "the other word's waiter was still parked and answerable after two wakes of its \
         bucket-mate, and futex_wake answered {other}",
    );

    for waiter in waiters {
        waiter.join().expect("a word waiter panicked");
    }
    sibling.join().expect("the sibling waiter panicked");
    println!("  counts: a count-limited wake names its word and says how many");
}

static CLAIM_WORD: AtomicU32 = AtomicU32::new(0);
static CLAIM_RETURNED: AtomicU32 = AtomicU32::new(0);
/// [`claim_semantics`]' waiters that have reached their `futex_wait`.
static CLAIM_ARMED: AtomicU32 = AtomicU32::new(0);
/// [`orphaned_by_unmap`]'s waiters that have reached their `futex_wait`.
static ORPHANS_ARMED: AtomicU32 = AtomicU32::new(0);
static SPINNERS_STOP: AtomicU32 = AtomicU32::new(0);

/// How many times the arrangement below is rebuilt before its own failure to
/// hold is the verdict.
const ATTEMPTS: usize = 6;

/// The claim arithmetic, on a schedule this test arranges rather than one it
/// hopes for.
///
/// Two things `Watch::post_n` must do are invisible unless a woken waiter
/// is **still on the bucket** when the next call walks it: a claim another call
/// already took must not be *counted*, and it must not *spend* the caller's
/// `limit`. A waiter leaves the bucket when it runs and returns, so both
/// questions are about the window between "the kernel claimed it" and "it ran"
/// — and that window used to be whatever the machine felt like giving.
///
/// **The window is the scheduler's own contract, not a sleep.** An ordinary
/// wake is `Urgency::Normal`, which `toyos_sched::mailbox` defines as "a busy
/// target drains at its next safe point (≤ one quantum, matching today's
/// contract) and needs no interrupt; a sleeping target is always kicked". One
/// spinner per CPU is therefore the whole arrangement: no CPU is sleeping, so
/// no wake sends an IPI, and a task the kernel has made `Ready` cannot be
/// dispatched until a quantum ends — 10 ms, against the microseconds the three
/// wakes below take between them.
///
/// The waiters park **before** the spinners start, and are proved parked by the
/// roster, as [`counts`]' are: a probe is a wake, and the waiters it claims are
/// still on the bucket when the first wake below walks it. Nothing posts to
/// their word between that proof and the first wake below, so "both are
/// parked" is not a guess. What is left is the one thing
/// the arrangement cannot make impossible — a quantum boundary landing inside
/// those microseconds — and that is *checked* rather than assumed: a waiter
/// that got out shows up in [`CLAIM_RETURNED`], and the attempt is rebuilt
/// instead of asserted on.
fn claim_semantics(cap: &SysCap) {
    for attempt in 1..=ATTEMPTS {
        CLAIM_WORD.store(0, Ordering::SeqCst);
        CLAIM_RETURNED.store(0, Ordering::SeqCst);
        CLAIM_ARMED.store(0, Ordering::SeqCst);
        let waiters: Vec<_> = (0..2)
            .map(|_| {
                thread::spawn(|| {
                    CLAIM_ARMED.fetch_add(1, Ordering::SeqCst);
                    unsafe { syscall::futex_wait(CLAIM_WORD.as_ptr(), 0, None) };
                    CLAIM_RETURNED.fetch_add(1, Ordering::SeqCst);
                })
            })
            .collect();
        await_armed_and_blocked(cap, &CLAIM_ARMED, 2);

        // Now make every CPU busy, so nothing the three wakes claim can be
        // dispatched before the last of them has run: this thread runs on one,
        // and the roster shows a spinner running on every other.
        let spinners = start_spinners();
        let others = syscall::cpu_count() as usize - 1;
        roster::await_true(|| {
            roster::my_threads(cap)
                .iter()
                .filter(|&&(is_thread, state)| is_thread && state == roster::RUNNING)
                .count()
                == others
        });

        // Changed first, so a waiter that does get out goes home and says so
        // rather than re-parking invisibly.
        CLAIM_WORD.store(1, Ordering::SeqCst);
        let first = unsafe { syscall::futex_wake(CLAIM_WORD.as_ptr(), 1) };
        let second = unsafe { syscall::futex_wake(CLAIM_WORD.as_ptr(), 1) };
        let third = unsafe { syscall::futex_wake(CLAIM_WORD.as_ptr(), 10) };
        let ran = CLAIM_RETURNED.load(Ordering::SeqCst);

        stop_spinners(spinners);
        for waiter in waiters {
            waiter.join().expect("a claim waiter panicked");
        }

        if ran != 0 && attempt < ATTEMPTS {
            continue;
        }
        assert_eq!(
            ran, 0,
            "a waiter was dispatched between the three wakes on all {ATTEMPTS} attempts, so \
             the state the two assertions below are about was never staged",
        );
        assert_eq!(
            first, 1,
            "futex_wake(count=1) on two parked waiters answered {first}",
        );
        // **A lost claim does not spend `limit`.** The first call took one
        // waiter's rendezvous word; this one walks that waiter first, must find
        // its claim gone, and must go on to the second waiter with its single
        // wake still in hand. A tree that charges the lost claim answers 0 here
        // and leaves the second waiter parked, which is a signal that woke
        // nobody.
        assert_eq!(
            second, 1,
            "futex_wake(count=1) answered {second}: the waiter it walked first was already \
             claimed, and a claim that is lost must cost the caller nothing — the wake was \
             for whoever is still parked",
        );
        // **The count is claims won, not waiters told.** Both waiters are on
        // the bucket and both are already claimed, so there is nobody here for
        // a third call to wake. A tree that counts what it told answers 2, and
        // that is how one thread gets reported to two callers.
        assert_eq!(
            third, 0,
            "futex_wake(count=10) answered {third} with both waiters already claimed and not \
             yet run — telling a waiter is not waking it, and counting it is how one thread \
             is reported twice",
        );
        println!("  claims: a claim already taken is neither counted nor charged");
        return;
    }
}

/// Wake `word` until the kernel answers that it claimed `want` waiters, with no
/// deadline: waiters that never park are a hang the harness ceiling reds.
///
/// The word must still hold what the waiters are waiting for, so every waiter
/// this probe wakes re-checks and re-parks; what it costs is a round trip, and
/// what it buys is proof that all of them had parked at one instant. Tree-
/// independent by construction: a wake with a limit above `want` over `want`
/// parked and unclaimed waiters answers `want` under every arithmetic this file
/// is about.
fn wait_until_parked(word: &AtomicU32, want: u64) {
    wait_until_parked_raw(word.as_ptr(), want);
}

/// Until `armed` reads `want` and the roster shows `want` of this process's
/// threads blocked, with no deadline. Each waiter counts itself into `armed`
/// with nothing but its `futex_wait` after, and they are the only threads of
/// this process that block, so a blocked one is blocked there.
fn await_armed_and_blocked(cap: &SysCap, armed: &AtomicU32, want: u32) {
    roster::await_true(|| {
        armed.load(Ordering::SeqCst) == want
            && roster::my_threads(cap)
                .iter()
                .filter(|&&(is_thread, state)| is_thread && state == roster::BLOCKED)
                .count()
                == want as usize
    });
}

/// One never-yielding thread per CPU, so the kernel has no sleeping target to
/// kick and no free CPU to dispatch a woken task onto.
fn start_spinners() -> Vec<thread::JoinHandle<()>> {
    SPINNERS_STOP.store(0, Ordering::SeqCst);
    (0..syscall::cpu_count())
        .map(|_| {
            thread::spawn(|| {
                while SPINNERS_STOP.load(Ordering::Relaxed) == 0 {
                    std::hint::spin_loop();
                }
            })
        })
        .collect()
}

fn stop_spinners(spinners: Vec<thread::JoinHandle<()>>) {
    SPINNERS_STOP.store(1, Ordering::SeqCst);
    for spinner in spinners {
        spinner.join().expect("a spinner panicked");
    }
}

/// A wait whose word is unmapped ends there, and the frame carries no claim
/// away with it.
///
/// The sibling doing the unmapping is this thread; the waiters are threads of
/// this process parked on words in the mapping; the sweeper is another
/// *process*, spawned before any of it so that its own start-up allocations
/// cannot be what consumes the frames. Its answers and the waiters' return are
/// two halves of one verdict and neither can be dropped: a tree that keeps the
/// stale nodes either has one of them stolen by the sweeper — a wake reported
/// to a process with nothing parked — or, if no frame came back its way, leaves
/// every waiter here parked on memory that is gone.
fn orphaned_by_unmap(cap: &SysCap) {
    let mut sweeper = Command::new(SELF)
        .arg(SWEEP)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn the sweeper: {e}"));
    let mut go = sweeper.stdin.take().expect("the sweeper's stdin is a pipe");

    let regions: Vec<usize> = (0..STALE_FRAMES)
        .map(|i| {
            let ptr = unsafe {
                syscall::mmap(
                    std::ptr::null_mut(),
                    PAGE_2M,
                    MmapProt::READ | MmapProt::WRITE,
                    MmapFlags::ANONYMOUS | MmapFlags::PRIVATE,
                )
            };
            assert!(!ptr.is_null(), "mmap #{i} for a futex word failed");
            // The word is the first in the region, so its offset in the frame
            // is zero — which is the offset the sweeper asks about.
            unsafe { (ptr as *mut u32).write(0) };
            ptr as usize
        })
        .collect();

    let waiters: Vec<_> = regions
        .iter()
        .map(|&addr| {
            thread::spawn(move || {
                ORPHANS_ARMED.fetch_add(1, Ordering::SeqCst);
                unsafe { syscall::futex_wait(addr as *const u32, 0, None) };
            })
        })
        .collect();
    // By the roster and not by a probe: a waiter a probe woke is still on its
    // way back to its word when the unmap below looks for it.
    await_armed_and_blocked(cap, &ORPHANS_ARMED, STALE_FRAMES as u32);

    // The unmap. Every frame behind these regions goes back to the PMM here,
    // and `mmap` hands the lowest free one to the next asker.
    for &addr in &regions {
        unsafe { syscall::munmap(addr as *mut u8, PAGE_2M) }.expect("munmap a futex region");
    }

    go.write_all(b"go\n").expect("tell the sweeper to go");
    drop(go);
    let status = sweeper.wait().expect("wait for the sweeper");
    assert!(
        status.success(),
        "the sweeper found a wake of its own fresh memory answered for by somebody else's \
         parked thread (exit={})",
        status.code().unwrap_or(-1),
    );

    // No deadline: nothing can ever post to a frame that has gone back to the
    // PMM, so a wait an unmap orphans and does not end is a hang the harness
    // ceiling reds.
    for waiter in waiters {
        waiter.join().expect("an orphaned waiter panicked");
    }
    println!("  unmap: {STALE_FRAMES} orphaned waits ended, and no wake outlived its frame");
}

/// [`wait_until_parked`] for a word that is not a `static`.
fn wait_until_parked_raw(word: *const u32, want: u64) {
    // A pace, and never a verdict: the probe's wake re-parks the waiters it
    // finds, so each look waits for them to be back.
    while unsafe { syscall::futex_wake(word, 10) } != want {
        thread::sleep(Duration::from_millis(10));
    }
}

/// A wait a revoke ended stays ended while its word still reads `expected`.
///
/// The revoke is another process letting go of its own window onto the same
/// frame: that unmap ends every wait on the frame, but this process's window
/// is still there and its word unchanged, so the waiter re-reads a condition
/// that is still false. Its registration is gone and no post can reach it
/// again, so it must return rather than park — or, as the kernel's loop once
/// could, go round its phase 1 forever.
fn revoked_while_still_mapped() {
    let ends = syscall::pipe().expect("a pipe");
    let window = syscall::pipe_map(ends.read).expect("map the pipe here");
    // Ring data nothing writes: nothing is ever written to this pipe.
    let word = window.wrapping_add(4096) as usize;
    let expected = unsafe { (word as *const u32).read_volatile() };

    let waiter = thread::spawn(move || unsafe { syscall::futex_wait(word as *const u32, expected, None) });
    // A wake to an unchanged word re-parks inside the call, so the
    // registration the revoke must find is still there.
    wait_until_parked_raw(word as *const u32, 1);

    let child = Command::new(SELF)
        .arg(LET_GO)
        .inherit_handle(INHERITED_PIPE.0, ends.read.0)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn the child that lets go: {e}"));
    let out = child.wait_with_output().expect("wait for the child that lets go");
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && said.contains("mapped and let go"),
        "the child never mapped the pipe and let go of it, so nothing was revoked \
         (exit={:?}):\n{said}",
        out.status.code(),
    );

    // No deadline: a waiter whose registration the revoke ended and that goes
    // round its phase 1 for ever, its word still mapped and unchanged, is a
    // hang the harness ceiling reds.
    waiter.join().expect("the revoked waiter panicked");
    syscall::close(ends.read);
    syscall::close(ends.write);
    println!("  revoke: a revoked wait ended with its word still mapped and unchanged");
}

/// The other process of [`revoked_while_still_mapped`]: map the pipe it
/// inherited, then close its only handle to it, which unmaps its window.
fn let_go() {
    syscall::pipe_map(INHERITED_PIPE).expect("map the inherited pipe");
    syscall::close(INHERITED_PIPE);
    println!("mapped and let go");
}

/// The other process: map fresh frames and ask each one's first word how many
/// waiters it has.
///
/// Every answer must be zero. This process has never called `futex_wait`, so a
/// non-zero answer is a wake it was credited with for a thread in the process
/// that just gave the frame back — the exact accounting a stale physical token
/// produces, and the exact wake that thread never gets.
fn sweep() {
    let mut go = [0u8; 8];
    // Blocking on the parent's signal rather than sleeping: the frames under
    // test are only the lowest free ones between the parent's unmap and the
    // first mmap below.
    let _ = std::io::Read::read(&mut std::io::stdin(), &mut go).expect("read the go signal");

    let mut stolen: Vec<(usize, u64)> = Vec::new();
    let mut regions = Vec::new();
    for i in 0..SWEEP_FRAMES {
        let ptr = unsafe {
            syscall::mmap(
                std::ptr::null_mut(),
                PAGE_2M,
                MmapProt::READ | MmapProt::WRITE,
                MmapFlags::ANONYMOUS | MmapFlags::PRIVATE,
            )
        };
        assert!(!ptr.is_null(), "sweeper mmap #{i} failed");
        let word = ptr as *const u32;
        let answered = unsafe { syscall::futex_wake(word, 1) };
        if answered != 0 {
            stolen.push((i, answered));
        }
        regions.push(ptr);
    }
    for ptr in regions {
        unsafe { syscall::munmap(ptr, PAGE_2M) }.expect("sweeper munmap");
    }
    assert!(
        stolen.is_empty(),
        "futex_wake on this process's own freshly mapped memory answered {stolen:?} — nothing \
         in this process has ever waited on a futex, so those are another process's waiters, \
         still armed on the physical address of a frame it gave back",
    );
    println!("  sweeper: {SWEEP_FRAMES} fresh frames, and no wake belonged to anybody else");
}
