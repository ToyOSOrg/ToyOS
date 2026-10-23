//! A thread's timing protection (`toyos::timing`, `SYS_TIMING_PROTECTION`):
//! what the kernel answers is each thread's own state, on whichever CPU the
//! thread is, and the scoped interface puts back what it found.
//!
//! **A guest, because the subject is the kernel's switch**: Intel's DOITM is a
//! register only Ring 0 writes, carried per thread across every switch and
//! migration, and Arm's `PSTATE.DIT` travels in the thread's saved state. No
//! host test reaches either. The query answers the register itself on x86-64
//! and the thread's saved `PSTATE` on AArch64.
//!
//! - **Never reported where absent**: a machine the kernel answers
//!   `NotSupported` gives inactive protections, and an x86-64 CPU without
//!   `IA32_ARCH_CAPABILITIES` (CPUID.(07H,0):EDX[29]) is answered so.
//! - **Scoped**: a protection turns the state on and puts back what it found,
//!   nested ones change nothing, and an early return and an unwinding panic
//!   end it. A program asks for none of it by any right, and `toyos-crypto`
//!   holds one while its secret work runs: a P-256 key's generation asks its
//!   caller for entropy inside it.
//! - **Each thread's own**: [`THREADS_PER_CPU`] threads a CPU, every other
//!   one protected, each sleeping and spinning [`ROUNDS`] times and asking
//!   its state after each. Every answer is the thread's own, and the
//!   scheduler's diary shows the premise: a protected thread picked on more
//!   than one CPU, and a CPU picking one of these threads after one in the
//!   other state.
//!
//! The `timing_protection_follows_its_thread` metal row holds the line it
//! prints on each bench; `virt_smp` runs it on AArch64.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::syscap::{SysCap, TraceCursor};
use toyos_abi::syscall::{self, SyscallError, TimingRequest};
use toyos_abi::trace::TraceRecord;
use toyos_trace::{Entry, Event, Thread};

const THREADS_PER_CPU: usize = 2;
const ROUNDS: usize = 200;
/// How long a round spins before it sleeps: long enough that a CPU with more
/// threads than it can run preempts some.
const SPIN: Duration = Duration::from_micros(300);
const SLEEP: Duration = Duration::from_micros(200);

/// The mode's name on this architecture, as the lines say it.
const MODE: &str = if cfg!(target_arch = "x86_64") { "DOITM" } else { "PSTATE.DIT" };

/// The calling thread's state as the kernel answers it: `None` where the
/// machine has none.
fn state() -> Option<bool> {
    match syscall::timing_protection(TimingRequest::Query) {
        Ok(on) => Some(on),
        Err(SyscallError::NotSupported) => None,
        Err(e) => panic!("SYS_TIMING_PROTECTION refused a query: {e}"),
    }
}

fn main() {
    let supported = state().is_some();
    if cfg!(target_arch = "x86_64") && !arch_capabilities() {
        assert!(!supported, "a CPU without IA32_ARCH_CAPABILITIES, which enumerates DOITM, was answered with a state");
    }
    assert_eq!(
        syscall::timing_protection(TimingRequest::On).map_err(|e| e == SyscallError::NotSupported),
        if supported { Ok(false) } else { Err(true) },
        "a thread that asked for nothing yet must start unprotected",
    );
    assert_eq!(state(), supported.then_some(true), "setting it was not what the next query read");
    assert_eq!(syscall::timing_protection(TimingRequest::Off).ok(), supported.then_some(true), "clearing it answered another state");
    assert_eq!(state(), supported.then_some(false));

    scoped(supported);
    crypto_holds_one(supported);
    let (migrated, changes) = each_threads_own(supported);
    if supported {
        println!(
            "timing_protection: {MODE} followed {} threads across {changes} switches between states, \
             and {migrated} protected threads ran on more than one CPU; nesting and early exits restore it",
            threads(),
        );
    } else {
        println!(
            "timing_protection: no timing protection on this machine, none reported, and {} threads read none",
            threads(),
        );
    }
}

/// Whether CPUID says this CPU has `IA32_ARCH_CAPABILITIES`.
#[cfg(target_arch = "x86_64")]
fn arch_capabilities() -> bool {
    core::arch::x86_64::__cpuid_count(7, 0).edx & 1 << 29 != 0
}

#[cfg(not(target_arch = "x86_64"))]
fn arch_capabilities() -> bool {
    true
}

/// Nesting, an early return and a panic each leave the state they found.
fn scoped(supported: bool) {
    let off = supported.then_some(false);
    let on = supported.then_some(true);
    {
        let outer = toyos::timing::protect();
        assert_eq!(outer.is_active(), supported, "a protection's activity is not the machine's support");
        assert_eq!(state(), on);
        {
            let inner = toyos::timing::protect();
            assert_eq!(inner.is_active(), supported);
            assert_eq!(state(), on);
        }
        assert_eq!(state(), on, "an inner protection's end ended the outer one");
    }
    assert_eq!(state(), off, "the outer protection's end left the state on");

    fn early(fail: bool) -> Result<(), ()> {
        let _protection = toyos::timing::protect();
        if fail {
            Err(())?;
        }
        Ok(())
    }
    assert_eq!(early(true), Err(()));
    assert_eq!(state(), off, "a `?` out of a protected scope left the state on");

    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let unwound = std::panic::catch_unwind(|| {
        let _protection = toyos::timing::protect();
        panic!("a panic inside a protection, caught on purpose");
    });
    std::panic::set_hook(hook);
    assert!(unwound.is_err());
    assert_eq!(state(), off, "a panic out of a protected scope left the state on");
}

/// Entropy whose every fill reads the state it is asked in.
struct Probe(Vec<Option<bool>>);

impl toyos_crypto::Random for Probe {
    fn fill(&mut self, out: &mut [u8]) -> Result<(), toyos_crypto::Error> {
        self.0.push(state());
        out.fill(7);
        Ok(())
    }
}

/// `toyos-crypto`'s secret work runs protected, and its protection nests in
/// its caller's.
fn crypto_holds_one(supported: bool) {
    let mut probe = Probe(Vec::new());
    toyos_crypto::ecc::p256::SecretKey::generate(&mut probe).expect("a scalar of sevens is below n");
    {
        let _batch = toyos::timing::protect();
        toyos_crypto::ecc::p256::SecretKey::generate(&mut probe).expect("a scalar of sevens is below n");
        assert_eq!(state(), supported.then_some(true), "toyos-crypto's protection ended its caller's");
    }
    assert_eq!(probe.0, [supported.then_some(true); 2], "toyos-crypto drew its secret unprotected");
    assert_eq!(state(), supported.then_some(false), "toyos-crypto left the state on");
}

fn threads() -> usize {
    syscall::cpu_count() as usize * THREADS_PER_CPU
}

/// Every other thread protected, each asking its own state after every
/// sleep and spin. Answers how many protected threads the diary shows on more
/// than one CPU, and how many times a CPU picked one of these threads after
/// one in the other state.
fn each_threads_own(supported: bool) -> (usize, usize) {
    let mut cursor = TraceCursor::new(vec![0; cap().trace_cursor_words().expect("the estate's capability sizes a cursor")]);
    read_to_now(&mut cursor);
    let protected: Arc<Mutex<BTreeMap<u32, bool>>> = Arc::default();
    let workers: Vec<_> = (0..threads())
        .map(|i| {
            let protected = Arc::clone(&protected);
            std::thread::spawn(move || {
                let wants = i % 2 == 0;
                protected.lock().unwrap().insert(toyos_abi::current_tid().raw(), wants);
                let protection = wants.then(toyos::timing::protect);
                for round in 0..ROUNDS {
                    let begun = Instant::now();
                    while begun.elapsed() < SPIN {
                        std::hint::spin_loop();
                    }
                    std::thread::sleep(SLEEP);
                    assert_eq!(
                        state(),
                        supported.then_some(wants),
                        "thread {i}, which asked for {wants}, read another state in round {round}",
                    );
                }
                drop(protection);
                assert_eq!(state(), supported.then_some(false), "thread {i} ended its protection and read it on");
            })
        })
        .collect();
    for worker in workers {
        worker.join().expect("a worker read another thread's state");
    }
    if !supported {
        return (0, 0);
    }

    let pid = std::process::id();
    let protected = protected.lock().unwrap();
    let mut cpus: BTreeMap<u32, BTreeSet<u16>> = BTreeMap::new();
    let mut last: BTreeMap<u16, bool> = BTreeMap::new();
    let mut changes = 0;
    for entry in read_to_now(&mut cursor) {
        let Some(Thread { pid: p, tid }) = entry.thread else { continue };
        let Some(&wants) = protected.get(&tid).filter(|_| p == pid && entry.event == Event::Pick) else { continue };
        if wants {
            cpus.entry(tid).or_default().insert(entry.cpu);
        }
        if last.insert(entry.cpu, wants).is_some_and(|before| before != wants) {
            changes += 1;
        }
    }
    let migrated = cpus.values().filter(|on| on.len() > 1).count();
    if syscall::cpu_count() > 1 {
        assert!(migrated > 0, "no protected thread was picked on a second CPU, so no migration was tested: {cpus:?}");
    }
    assert!(changes > 0, "no CPU picked a thread after one in the other state, so no switch between them was tested");
    (migrated, changes)
}

/// The estate's capability, taken once.
fn cap() -> &'static SysCap {
    static CAP: std::sync::OnceLock<SysCap> = std::sync::OnceLock::new();
    CAP.get_or_init(|| Endowments::get().take(SYSCAP_LABEL).expect("test-runner endows a capability"))
}

/// Every record from `cursor` on, until a read leaves its buffer short.
fn read_to_now(cursor: &mut TraceCursor<Vec<u64>>) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut raw = vec![TraceRecord::EMPTY; 512];
    loop {
        let n = cap().trace(cursor, &mut raw).expect("the estate's capability reads the diary");
        out.extend(raw[..n].iter().filter_map(|r| Entry::decode(r).ok()));
        if n < raw.len() {
            return out;
        }
    }
}
