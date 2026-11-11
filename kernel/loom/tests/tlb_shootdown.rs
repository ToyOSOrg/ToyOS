//! Loom: the shootdown acknowledgement's happens-before edges.
//!
//! The property under test is the one the whole stage exists for:
//!
//! > when an initiator is told that cpu 1 has flushed, cpu 1's translations
//! > already reflect the page-table write the initiator made before asking.
//!
//! A TLB is modelled as one cell holding what cpu 1's last page walk found, and
//! a flush is modelled as re-walking — the strongest thing a flush can be asked
//! to mean and the only one expressible here. So `UNMAPPED` after an
//! acknowledged shootdown is "cpu 1 can no longer reach the page the initiator
//! is about to free", and `MAPPED` is the use-after-free this stage removes.
//! A kernel-half entry is a second cell, re-walked only by a wide answer, as a
//! global translation is dropped only by the flush that drops global ones.
//!
//! The edge the property rests on is `serve`'s load of what it owes: it
//! synchronizes with `issue`'s release, and that is what puts the initiator's
//! page-table write ahead of the flush. On x86 every load is an acquire, so a
//! build with it relaxed behaves identically to this one and no guest test can
//! fail here. The negative case is a cargo feature rather than a comment:
//!
//! ```text
//! cargo test --manifest-path kernel/loom/Cargo.toml --features shootdown-serve-relaxed \
//!   --test tlb_shootdown
//! ```
//!
//! makes that load relaxed and this file must red — *cpu 1 acknowledged the
//! shootdown while still holding a translation for the page the initiator is
//! about to free*, which is the use-after-free this stage removes, stated
//! exactly. Verified 2026-08-17, both ways round.
//!
//! **Every spin is bounded and every assertion is conditional on the ack having
//! arrived, so [`acknowledged_model`] stops the models passing vacuously.** An
//! unbounded serve loop is the same trap `kernel-loom`'s lock models document:
//! loom explores it as an unbounded branch and never finishes. Safety is what
//! the bounded form checks, and safety is the whole property — liveness in the
//! kernel comes from the IPI being redelivered, which is hardware and not this
//! protocol.

#![cfg(feature = "loom")]

use std::sync::atomic::{AtomicBool, Ordering::SeqCst};

use kernel_loom::shootdown::{Asks, CpuSet, Flush, Reach, Shootdown};
use loom::sync::Arc;
use loom::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// The page-table entry every model writes: 1 mapped, 0 unmapped.
const MAPPED: u64 = 1;
const UNMAPPED: u64 = 0;

/// How many times cpu 1 takes the interrupt. Two, because the first may land
/// before the initiator's `issue` is visible and publish a generation that does
/// not answer it — which is a schedule the kernel resolves by the vector staying
/// pending, and which this bound stands in for.
///
/// A `while` loop waiting for the IPI would be truer to the kernel and is not
/// available: loom explores a spin as an unbounded branch and does not finish —
/// measured here, twice, before this became a counted loop.
const SERVES: usize = 2;

/// How many times the initiator looks for its acknowledgement.
const POLLS: usize = 3;

/// Every model here explores interleavings with at most this many preemptions.
///
/// Unbounded, neither model finishes: measured at over seven minutes each with
/// no verdict, which is the same wall this crate's lock models hit. Bounded at
/// two, both run in about ten seconds and both negative controls are still
/// caught — which is the check that matters, because a bound that hides the
/// controls would be a bound that hides the defect too.
const PREEMPTIONS: usize = 2;

fn model(f: impl Fn() + Sync + Send + 'static) {
    let mut builder = loom::model::Builder::new();
    builder.preemption_bound = Some(PREEMPTIONS);
    builder.check(f);
}

/// [`model`] over `f`, which answers whether its interleaving completed a
/// wait, failing unless one did.
///
/// The witness is outside the closure because loom re-runs it once per
/// interleaving, and is this call's own because the tests run in parallel in
/// one binary. Without it, bounding the spins would make every assertion
/// skippable and a broken protocol could pass by never being asked.
fn acknowledged_model(f: impl Fn() -> bool + Sync + Send + 'static) {
    let acked = std::sync::Arc::new(AtomicBool::new(false));
    let witness = std::sync::Arc::clone(&acked);
    model(move || {
        if f() {
            witness.store(true, SeqCst);
        }
    });
    assert!(acked.load(SeqCst), "no interleaving completed a wait, so the assertions never ran");
}

struct Machine {
    shootdown: Shootdown,
    /// The user-half page-table entry the initiator unmaps.
    pte: AtomicU64,
    /// What cpu 1's last page walk of it found. Every flush re-walks.
    tlb: AtomicU64,
    /// The kernel-half entry, whose translation is global.
    kernel_pte: AtomicU64,
    /// What cpu 1's last walk of it found. Only a wide flush re-walks.
    kernel_tlb: AtomicU64,
}

impl Machine {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            shootdown: two_cpus(),
            pte: AtomicU64::new(MAPPED),
            tlb: AtomicU64::new(MAPPED),
            kernel_pte: AtomicU64::new(MAPPED),
            kernel_tlb: AtomicU64::new(MAPPED),
        })
    }

    /// cpu 1's flush, as far as `reach` asks.
    fn flush(&self, reach: Reach) {
        self.tlb.store(self.pte.load(Relaxed), Relaxed);
        if reach == Reach::Wide {
            self.kernel_tlb.store(self.kernel_pte.load(Relaxed), Relaxed);
        }
    }

    /// One serve on cpu 1: vector 0xFE's body serves when owed, and serving
    /// regardless explores every schedule that one does and more.
    fn serve(&self) {
        self.shootdown.serve(1, |reach| self.flush(reach));
    }

    /// cpu 1's side: take the interrupt and run vector 0xFE's body.
    fn take_interrupts(&self) {
        for _ in 0..SERVES {
            loom::thread::yield_now();
            self.serve();
        }
    }

    /// The initiator's side, up to the point where it would free the pages:
    /// `pte` written for a user-half change, `kernel_pte` for a kernel-half
    /// one, and what cpu 1 holds of it once it has answered. `None` when the
    /// bounded poll ran out, which is a schedule this model says nothing about.
    fn unmap_and_wait(&self, reach: Reach) -> Option<u64> {
        let (pte, tlb) = match reach {
            Reach::Narrow => (&self.pte, &self.tlb),
            Reach::Wide => (&self.kernel_pte, &self.kernel_tlb),
        };
        pte.store(UNMAPPED, Relaxed);
        let generation = self.shootdown.issue(reach);
        for _ in 0..POLLS {
            if self.shootdown.served(1, generation) {
                return Some(tlb.load(Relaxed));
            }
            loom::thread::yield_now();
        }
        None
    }
}

/// An acknowledged shootdown means cpu 1 has re-walked since the unmap.
#[test]
fn an_acknowledged_flush_postdates_the_page_table_write() {
    acknowledged_model(|| {
        let m = Machine::new();

        let target = {
            let m = m.clone();
            loom::thread::spawn(move || m.take_interrupts())
        };

        let answer = m.unmap_and_wait(Reach::Narrow);
        if let Some(reachable) = answer {
            assert_eq!(
                reachable, UNMAPPED,
                "cpu 1 acknowledged the shootdown while still holding a \
                 translation for the page the initiator is about to free",
            );
        }

        target.join().unwrap();
        answer.is_some()
    });
}

/// Two initiators, one target: one serve may answer both, which is what makes a
/// single pending IPI bit sufficient.
///
/// The vector's IRR is one bit per CPU, so a second shootdown raised while the
/// handler runs coalesces with the first. That is only sound because a serve
/// publishes the *latest* generation it saw rather than the one that woke it —
/// this model is what says so.
#[test]
fn one_serve_answers_two_concurrent_shootdowns() {
    acknowledged_model(|| {
        let m = Machine::new();

        let target = {
            let m = m.clone();
            loom::thread::spawn(move || m.take_interrupts())
        };

        let second = {
            let m = m.clone();
            loom::thread::spawn(move || m.unmap_and_wait(Reach::Narrow))
        };

        let first = m.unmap_and_wait(Reach::Narrow);
        if let Some(reachable) = first {
            assert_eq!(reachable, UNMAPPED, "the first initiator freed a page cpu 1 could still reach");
        }
        let second = second.join().unwrap();
        if let Some(reachable) = second {
            assert_eq!(reachable, UNMAPPED, "the second initiator freed a page cpu 1 could still reach");
        }

        target.join().unwrap();
        first.is_some() || second.is_some()
    });
}

/// A serve that an interrupt nests inside another publishes the later
/// generation, and the outer serve, finishing after it, must not take it back.
///
/// The nested serve runs on a thread of its own, so loom places it at every
/// point of the outer one, the nesting among them. Reds when `serve` stores
/// what it owes instead of raising to it.
#[test]
fn a_nested_serve_is_not_undone_by_the_one_it_interrupted() {
    model(|| {
        let s = Arc::new(two_cpus());
        let first = s.issue(Reach::Narrow);
        let nested = {
            let s = Arc::clone(&s);
            loom::thread::spawn(move || {
                let later = s.issue(Reach::Narrow);
                s.serve(1, |_| {});
                later
            })
        };
        s.serve(1, |_| {});
        let later = nested.join().unwrap();
        assert!(s.served(1, first));
        assert!(
            s.served(1, later),
            "cpu 1 flushed for the nested shootdown, and the serve it interrupted \
             published an older generation over it",
        );
    });
}

/// A kernel-half shootdown acknowledged means cpu 1 has dropped its global
/// translation since the write.
///
/// The edge is `issue`'s count of wide generations, raised before the
/// generation exists: a serve that owes the generation reads the count, so its
/// answer is wide. Reds when the count is raised after the generation is
/// issued, where a serve can owe it and still answer narrow.
#[test]
fn an_acknowledged_kernel_half_shootdown_dropped_the_global_translation() {
    acknowledged_model(|| {
        let m = Machine::new();

        let target = {
            let m = m.clone();
            loom::thread::spawn(move || m.take_interrupts())
        };

        let answer = m.unmap_and_wait(Reach::Wide);
        if let Some(reachable) = answer {
            assert_eq!(
                reachable, UNMAPPED,
                "cpu 1 acknowledged a kernel-half shootdown while still holding the \
                 global translation the initiator changed",
            );
        }

        target.join().unwrap();
        answer.is_some()
    });
}

/// One serve may answer a user-half and a kernel-half shootdown at once, and
/// its answer is then the wide one: the user-half initiator's page is gone
/// either way, and the kernel-half initiator's global translation with it.
#[test]
fn one_serve_answering_a_user_half_and_a_kernel_half_shootdown_answers_wide() {
    acknowledged_model(|| {
        let m = Machine::new();

        let target = {
            let m = m.clone();
            loom::thread::spawn(move || m.take_interrupts())
        };

        let kernel = {
            let m = m.clone();
            loom::thread::spawn(move || m.unmap_and_wait(Reach::Wide))
        };

        let first = m.unmap_and_wait(Reach::Narrow);
        if let Some(reachable) = first {
            assert_eq!(reachable, UNMAPPED, "the user-half initiator freed a page cpu 1 could still reach");
        }
        let kernel = kernel.join().unwrap();
        if let Some(reachable) = kernel {
            assert_eq!(reachable, UNMAPPED, "the kernel-half initiator's global translation outlived cpu 1's answer");
        }

        target.join().unwrap();
        first.is_some() || kernel.is_some()
    });
}

/// A serve nested inside a wide one that has not yet flushed answers wide
/// itself: the record of a wide answer is raised only once its flush is done.
///
/// The two serves are two threads on cpu 1, so loom places the nested one at
/// every point of the outer one. Reds when the record is raised before the
/// flush, where the nested serve reads it, answers narrow, and publishes the
/// generation while the global translation is still held.
#[test]
fn a_serve_nested_in_an_unfinished_wide_one_answers_wide() {
    acknowledged_model(|| {
        let m = Machine::new();

        let outer = {
            let m = m.clone();
            loom::thread::spawn(move || m.serve())
        };
        let nested = {
            let m = m.clone();
            loom::thread::spawn(move || m.serve())
        };

        let answer = m.unmap_and_wait(Reach::Wide);
        if let Some(reachable) = answer {
            assert_eq!(
                reachable, UNMAPPED,
                "a nested serve published the kernel-half shootdown while the wide \
                 answer it nested in had not yet dropped the global translation",
            );
        }

        outer.join().unwrap();
        nested.join().unwrap();
        answer.is_some()
    });
}

/// A serve owing only user-half shootdowns answers narrow, before and after a
/// kernel-half one, which it answers wide once: a user-half change keeps the
/// kernel's global translations.
///
/// **Not an interleaving question**: `loom::model` wraps it because this
/// crate's atomics are loom's. Reds when a serve answers wide with no wide
/// generation owed.
#[test]
fn only_a_kernel_half_shootdown_asks_a_wide_answer() {
    model(|| {
        let s = two_cpus();
        let reach_of = |s: &Shootdown| {
            let mut answered = None;
            s.serve(1, |reach| answered = Some(reach));
            answered.expect("serve runs its flush")
        };

        s.issue(Reach::Narrow);
        assert_eq!(reach_of(&s), Reach::Narrow, "a user-half shootdown was answered wide");
        s.issue(Reach::Wide);
        s.issue(Reach::Narrow);
        assert_eq!(reach_of(&s), Reach::Wide, "a kernel-half shootdown was answered narrow");
        s.issue(Reach::Narrow);
        assert_eq!(
            reach_of(&s),
            Reach::Narrow,
            "a user-half shootdown after an answered kernel-half one was answered wide",
        );
    });
}

/// A round between the two CPUs every model here names.
fn two_cpus() -> Shootdown {
    let shootdown = Shootdown::new();
    shootdown.size(2);
    shootdown
}

/// A user change's range asked of the CPUs running its space ([`Asks`]): cpu 1
/// is the target and holds a translation of each of two pages, page 0 and
/// page 1, as one cell each. A range answer re-walks the pages it names, an
/// answer to more than one ask at once every page.
struct Asked {
    asks: Asks,
    pte: [AtomicU64; 2],
    tlb: [AtomicU64; 2],
}

impl Asked {
    fn new() -> Arc<Self> {
        let asks = Asks::new();
        asks.size(3);
        Arc::new(Self {
            asks,
            pte: [AtomicU64::new(MAPPED), AtomicU64::new(MAPPED)],
            tlb: [AtomicU64::new(MAPPED), AtomicU64::new(MAPPED)],
        })
    }

    fn rewalk(&self, page: usize) {
        self.tlb[page].store(self.pte[page].load(Relaxed), Relaxed);
    }

    /// cpu 1's answers, `SERVES` of them.
    fn take_interrupts(&self) {
        for _ in 0..SERVES {
            loom::thread::yield_now();
            self.asks.serve(1, |what| match what {
                Flush::Range { start, end, .. } => (start..end.min(2)).for_each(|page| self.rewalk(page as usize)),
                Flush::User => (0..2).for_each(|page| self.rewalk(page)),
            });
        }
    }

    /// `me` unmaps `page` and asks cpu 1 to flush it; what cpu 1 holds of the
    /// page once it has answered, `None` when the bounded poll ran out.
    fn unmap_and_ask(&self, me: usize, page: usize) -> Option<u64> {
        self.pte[page].store(UNMAPPED, Relaxed);
        let mut targets = CpuSet::default();
        targets[0] = 1 << 1;
        self.asks.ask(me, 0, page as u64, page as u64 + 1, &targets);
        for _ in 0..POLLS {
            if self.asks.answered(me, 1) {
                return Some(self.tlb[page].load(Relaxed));
            }
            loom::thread::yield_now();
        }
        None
    }
}

/// An answered ask means cpu 1 has re-walked the page asked after the unmap.
///
/// Reds when the target takes its asks `Relaxed`
/// (`shootdown-take-relaxed`), so its flush no longer follows the write; when
/// it answers `Relaxed` (`shootdown-answer-relaxed`), so the asking CPU can
/// read its answer before the flush; and when the asking CPU writes its range
/// after asking (`shootdown-ask-range-late`), so the target can flush a range
/// it was never asked.
#[test]
fn an_answered_ask_flushed_its_range_after_the_write() {
    acknowledged_model(|| {
        let m = Asked::new();
        let target = {
            let m = m.clone();
            loom::thread::spawn(move || m.take_interrupts())
        };
        let answer = m.unmap_and_ask(0, 0);
        if let Some(reachable) = answer {
            assert_eq!(reachable, UNMAPPED, "cpu 1 answered the ask while still holding the page asked");
        }
        target.join().unwrap();
        answer.is_some()
    });
}

/// Two CPUs ask cpu 1 at once, each for its own page: an answer to one alone
/// flushes its range, an answer to both every page, and each asking CPU finds
/// its own page gone once answered, whichever way its ask was taken.
///
/// Reds when an answer to two asks at once flushes one of their ranges
/// (`shootdown-ask-many-as-one`).
#[test]
fn two_asks_of_one_cpu_are_each_answered_for_their_own_page() {
    acknowledged_model(|| {
        let m = Asked::new();
        let target = {
            let m = m.clone();
            loom::thread::spawn(move || m.take_interrupts())
        };
        let second = {
            let m = m.clone();
            loom::thread::spawn(move || m.unmap_and_ask(2, 1))
        };
        let first = m.unmap_and_ask(0, 0);
        if let Some(reachable) = first {
            assert_eq!(reachable, UNMAPPED, "cpu 0 was answered while cpu 1 still held page 0");
        }
        let second = second.join().unwrap();
        if let Some(reachable) = second {
            assert_eq!(reachable, UNMAPPED, "cpu 2 was answered while cpu 1 still held page 1");
        }
        target.join().unwrap();
        first.is_some() || second.is_some()
    });
}

/// A CPU asks again only once every CPU it asked has answered, and its next
/// ask waits for answers of its own: its earlier answers do not stand for it.
///
/// Reds when an ask leaves the previous ask's answers standing
/// (`shootdown-ask-stale-answers`): the mailbox reused would be answered at
/// once, while cpu 1 still holds the second page.
#[test]
fn a_mailbox_reused_waits_for_its_new_answer() {
    acknowledged_model(|| {
        let m = Asked::new();
        let target = {
            let m = m.clone();
            loom::thread::spawn(move || {
                m.take_interrupts();
                m.take_interrupts();
            })
        };
        let mut answered = false;
        if m.unmap_and_ask(0, 0).is_some() {
            let again = m.unmap_and_ask(0, 1);
            if let Some(reachable) = again {
                assert_eq!(reachable, UNMAPPED, "cpu 0's second ask was answered while cpu 1 still held page 1");
            }
            answered = again.is_some();
        }
        target.join().unwrap();
        answered
    });
}

/// A serve answers each CPU asking it once, with that CPU's own range: two
/// CPUs that ask each other each find their answer once the other has
/// served, and a second serve finds nothing left to answer. **Not an
/// interleaving question.** The order in which a waiting CPU answers before
/// it reads its own answer is `arch::tlb::wait_for`'s, which loom does not
/// compile; its doc states it and inspection holds it.
#[test]
fn each_asker_is_answered_once_with_its_own_range() {
    model(|| {
        let asks = Asks::new();
        asks.size(2);
        let (mut to_one, mut to_zero) = (CpuSet::default(), CpuSet::default());
        to_one[0] = 1 << 1;
        to_zero[0] = 1;
        asks.ask(0, 0, 0, 1, &to_one);
        asks.ask(1, 0, 1, 2, &to_zero);
        // cpu 0's serve answers cpu 1 with cpu 1's range.
        assert!(asks.serve(0, |what| assert_eq!(what, Flush::Range { tag: 0, start: 1, end: 2 })));
        assert!(!asks.answered(0, 1));
        assert!(asks.answered(1, 0), "cpu 0 served and did not answer cpu 1");
        assert!(asks.serve(1, |_| {}));
        assert!(asks.answered(0, 1));
        assert!(!asks.serve(0, |_| {}), "an ask was answered twice");
    });
}
