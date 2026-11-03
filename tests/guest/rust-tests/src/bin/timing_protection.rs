//! A thread's timing protection (`toyos::timing`): on AArch64 it is the
//! thread's own `PSTATE.DIT` on whichever CPU the thread runs, and the scoped
//! interface puts back what it found.
//!
//! **A guest, because the subject is the kernel's exception path**: EL0 sets
//! `PSTATE.DIT`, and only the kernel's save of `SPSR_EL1` on every exception
//! from EL0 and its return carry it across switches and migrations. No host
//! test reaches either. The state is read here with `MRS`, not through the
//! interface under test.
//!
//! - **Scoped**: a protection turns the state on and puts back what it found,
//!   nested ones change nothing, and an early return and an unwinding panic
//!   end it. A program asks for none of it by any right, and `toyos-crypto`
//!   holds one while its secret work runs: a P-256 key's generation asks its
//!   caller for entropy inside it.
//! - **Each thread's own**: [`THREADS_PER_CPU`] threads a CPU, every other
//!   one protected, each spinning and sleeping [`ROUNDS`] times and reading
//!   its state after each. Every reading is the thread's own, and the
//!   scheduler's diary shows the premise: a protected thread picked on more
//!   than one CPU, and a CPU picking one of these threads after one in the
//!   other state.
//!
//! `virt_smp` runs it on AArch64, whose emulated CPUs have FEAT_DIT.

#[path = "../arch/dit.rs"]
mod arch;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use arch::dit as state;
use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::syscap::{SysCap, TraceCursor};
use toyos_abi::syscall;
use toyos_abi::trace::TraceRecord;
use toyos_trace::{Entry, Event, Thread};

const THREADS_PER_CPU: usize = 2;
const ROUNDS: usize = 200;
/// How long a round spins before it sleeps: long enough that a CPU with more
/// threads than it can run preempts some.
const SPIN: Duration = Duration::from_micros(300);
const SLEEP: Duration = Duration::from_micros(200);

fn main() {
    let supported = state().is_some();
    assert_eq!(state(), supported.then_some(false), "a thread that asked for nothing must start unprotected");
    scoped(supported);
    crypto_holds_one(supported);
    let (migrated, changes) = each_threads_own(supported);
    if supported {
        println!(
            "timing_protection: PSTATE.DIT followed {} threads across {changes} switches between states, \
             and {migrated} protected threads ran on more than one CPU; nesting and early exits restore it",
            threads(),
        );
    } else {
        println!("timing_protection: no timing protection on this machine, and none reported");
    }
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

/// Every other thread protected, each reading its own state after every
/// spin and sleep. Answers how many protected threads the diary shows on more
/// than one CPU, and how many times a CPU picked one of these threads after
/// one in the other state.
fn each_threads_own(supported: bool) -> (usize, usize) {
    if !supported {
        return (0, 0);
    }
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
                    assert_eq!(state(), Some(wants), "thread {i}, which asked for {wants}, read another state in round {round}");
                }
                drop(protection);
                assert_eq!(state(), Some(false), "thread {i} ended its protection and read it on");
            })
        })
        .collect();
    for worker in workers {
        worker.join().expect("a worker read another thread's state");
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
