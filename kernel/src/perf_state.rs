//! A `perf-state` claim's read. A per-CPU register is readable only on its own
//! CPU, so the reading CPU answers for itself and asks the rest: it issues a
//! generation and kicks every other CPU, each answers from its next scheduler
//! pass ([`serve_if_owed`], from `drain_irqs`) by reading its registers into
//! its slot and posting [`WATCH`], and the read blocks on that watch until
//! every CPU has answered — bounded by [`ANSWER`], past which it is refused
//! `Io` and the silent CPUs are named.
//!
//! The ask and its answers are [`Shootdown`]'s protocol: an answer published
//! for a generation was read after that generation was issued. A slot that a
//! later ask overwrites mid-copy mixes two answers, each read after this one
//! asked.

use core::sync::atomic::{AtomicU64, Ordering::Relaxed};

use toyos_abi::perf::{answer_len, CpuRegisters};
use toyos_abi::syscall::SyscallError;

use crate::arch::perf_state::{self, Declared};
use crate::shootdown::{Generation, Shootdown, MAX_CPUS};
use crate::sync::Lock;
use crate::time::{Budget, Deadline, Duration, Instant};
use crate::user_ptr::UserBytesMut;
use crate::watch::Watch;

static ASKS: Shootdown = Shootdown::new();
static SLOTS: [Slot; MAX_CPUS] = [const { Slot::new() }; MAX_CPUS];

/// Posted by every answer; a blocked read waits here.
pub static WATCH: Watch = Watch::new();

/// A kicked CPU reaches a pass within one timer interrupt; this is the
/// kernel's choice, the blocked-task dump's for the same question.
const ANSWER: Budget = Budget::of(
    Duration::from_millis(250),
    "the read is refused `Io`, and the CPUs that did not answer are named",
);

struct Slot([AtomicU64; 5]);

impl Slot {
    const fn new() -> Self {
        Self([const { AtomicU64::new(0) }; 5])
    }

    fn store(&self, r: CpuRegisters) {
        let words = [r.pm_enable, r.hwp_capabilities, r.hwp_request, r.energy_perf_bias, r.misc_enable];
        for (slot, word) in self.0.iter().zip(words) {
            slot.store(word, Relaxed);
        }
    }

    fn load(&self) -> CpuRegisters {
        let [pm_enable, hwp_capabilities, hwp_request, energy_perf_bias, misc_enable] =
            self.0.each_ref().map(|word| word.load(Relaxed));
        CpuRegisters { pm_enable, hwp_capabilities, hwp_request, energy_perf_bias, misc_enable }
    }
}

/// One claim's side of the protocol: the proof its reads need, and the ask
/// its reads wait on — joined by every read of the claim until it is answered.
pub struct Reader {
    declared: Declared,
    pending: Lock<Option<Ask>>,
}

#[derive(Clone, Copy)]
struct Ask {
    generation: Generation,
    deadline: Deadline,
}

impl Reader {
    pub fn new(declared: Declared) -> Self {
        Self { declared, pending: Lock::new(None) }
    }

    /// The answer's bytes into `buf`, or `None` while a CPU has not answered —
    /// the caller then waits on [`WATCH`] until [`answered`].
    pub fn read(&self, buf: &mut UserBytesMut) -> Option<u64> {
        let cpus = crate::arch::smp::cpu_count() as usize;
        let len = answer_len(cpus);
        if buf.len() < len {
            return Some(SyscallError::ResourceExhausted.to_u64());
        }
        let now = crate::clock::now();
        let mut pending = self.pending.lock();
        let ask = *pending.get_or_insert_with(|| self.ask(cpus, now));
        let answered = (0..cpus).all(|cpu| ASKS.served(cpu, ask.generation));
        if !answered && !ask.deadline.reached(now) {
            return None;
        }
        *pending = None;
        drop(pending);
        if !answered {
            for cpu in (0..cpus).filter(|&cpu| !ASKS.served(cpu, ask.generation)) {
                crate::log!("perf_state: cpu{cpu} did not answer a read within {ANSWER}");
            }
            return Some(SyscallError::Io.to_u64());
        }
        buf.write_at(0, perf_state::read_package(&self.declared).as_bytes());
        // `answer_len(cpu)` is where CPU `cpu`'s record starts.
        for (cpu, slot) in SLOTS[..cpus].iter().enumerate() {
            buf.write_at(answer_len(cpu), slot.load().as_bytes());
        }
        Some(len as u64)
    }

    /// Issued, this CPU's answer given, and every other CPU kicked — in that
    /// order, so no kicked CPU can look before the generation it owes exists.
    fn ask(&self, cpus: usize, now: Instant) -> Ask {
        let generation = ASKS.issue();
        let me = crate::arch::percpu::cpu_id() as usize;
        ASKS.serve(me, || SLOTS[me].store(perf_state::read_cpu(&self.declared)));
        for cpu in (0..cpus).filter(|&cpu| cpu != me) {
            crate::arch::irqchip::kick_cpu(cpu as u32);
        }
        Ask { generation, deadline: Deadline::at(now + ANSWER.duration()) }
    }
}

/// Whether every CPU has answered the latest ask: a blocked read's wake
/// condition, and a hint — [`Reader::read`] decides.
pub fn answered() -> bool {
    (0..crate::arch::smp::cpu_count() as usize).all(|cpu| !ASKS.owes(cpu))
}

/// How long a blocked read parks before it looks again; its own ask's
/// deadline is what refuses it.
pub fn park_deadline() -> Deadline {
    Deadline::at(crate::clock::now() + ANSWER.duration())
}

/// This CPU's answer, if one is owed. Called from `drain_irqs` every pass, so
/// what it costs when nothing is owed is two relaxed loads.
pub fn serve_if_owed() {
    let me = crate::arch::percpu::cpu_id() as usize;
    if !ASKS.owes(me) {
        return;
    }
    // The proof is used inside the closure only: on an architecture where it
    // is uninhabited, binding one here would make the rest unreachable.
    let read = perf_state::declared()
        .map(|declared| move || perf_state::read_cpu(&declared))
        .expect("an ask is made only through a claim, and a claim only where the request is declared");
    ASKS.serve(me, || SLOTS[me].store(read()));
    WATCH.post();
}
