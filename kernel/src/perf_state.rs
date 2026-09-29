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
//!
//! **An ask belongs to the one read that made it** and lives on that read's
//! stack, so a read that ends — answered, refused, cancelled or not waiting —
//! leaves nothing a later read could be answered from or refused by.

use core::sync::atomic::AtomicU64;
use core::sync::atomic::Ordering::{Acquire, Relaxed, Release};

use toyos_abi::perf::{answer_len, CpuRegisters};
use toyos_abi::syscall::SyscallError;

use crate::arch::perf_state::{self, Declared};
use crate::shootdown::{Generation, Shootdown, MAX_CPUS};
use crate::time::{Budget, Deadline, Duration, Instant};
use crate::user_ptr::UserBytesMut;
use crate::watch::Watch;

static ASKS: Shootdown = Shootdown::new();
static SLOTS: [Slot; MAX_CPUS] = [const { Slot::new() }; MAX_CPUS];

/// Posted by every answer; a blocked read waits here.
pub static WATCH: Watch = Watch::new();

const ANSWER: Budget = Budget::of(
    Duration::from_millis(250),
    "the read is refused `Io`, and the CPUs that did not answer are named",
);

struct Slot([AtomicU64; 6]);

impl Slot {
    const fn new() -> Self {
        Self([const { AtomicU64::new(0) }; 6])
    }

    fn store(&self, r: CpuRegisters) {
        let words = [
            r.hardware_id,
            r.pm_enable,
            r.hwp_capabilities,
            r.hwp_request,
            r.energy_perf_bias,
            r.misc_enable,
        ];
        for (slot, word) in self.0.iter().zip(words) {
            slot.store(word, Relaxed);
        }
    }

    fn load(&self) -> CpuRegisters {
        let [hardware_id, pm_enable, hwp_capabilities, hwp_request, energy_perf_bias, misc_enable] =
            self.0.each_ref().map(|word| word.load(Relaxed));
        CpuRegisters {
            hardware_id,
            pm_enable,
            hwp_capabilities,
            hwp_request,
            energy_perf_bias,
            misc_enable,
        }
    }
}

/// One claim's side of the protocol: the proof its reads need.
pub struct Reader {
    /// `None` only under `perf-state-deaf-cpu`, whose claim has no
    /// declaration behind it and answers each CPU's identity and zeros.
    declared: Option<Declared>,
}

/// One read's ask, made by its first look and gone with the read.
#[derive(Clone, Copy)]
pub struct Ask {
    generation: Generation,
    deadline: Deadline,
}

impl Ask {
    /// Issued, this CPU's answer given, and every other CPU kicked — in that
    /// order, so no kicked CPU can look before the generation it owes exists.
    fn issue(cpus: usize, now: Instant) -> Self {
        let generation = ASKS.issue();
        if crate::actuator::perf_state_deaf_cpu() {
            DEAF_ASKED.fetch_add(1, Release);
        }
        let me = crate::arch::percpu::cpu_id() as usize;
        ASKS.serve(me, || SLOTS[me].store(sample(me)));
        for cpu in (0..cpus).filter(|&cpu| cpu != me) {
            crate::arch::irqchip::kick_cpu(cpu as u32);
        }
        Self { generation, deadline: Deadline::at(now + ANSWER.duration()) }
    }

    /// Whether every CPU has answered this ask: a blocked read's wake condition.
    pub fn answered(&self) -> bool {
        (0..crate::arch::smp::cpu_count() as usize).all(|cpu| ASKS.served(cpu, self.generation))
    }

    /// Past this the read is refused, so its wait ends here.
    pub fn deadline(&self) -> Deadline {
        self.deadline
    }
}

impl Reader {
    /// A claim's reader, or why this machine has none.
    pub fn claim() -> Result<Self, &'static str> {
        let declared = perf_state::declared().map(Some).or_else(|why| {
            if crate::actuator::perf_state_deaf_cpu() { Ok(None) } else { Err(why) }
        })?;
        Ok(Self { declared })
    }

    /// The answer's bytes into `buf`, or `None` while a CPU has not answered
    /// `ask` — the caller then waits on [`WATCH`] until [`Ask::answered`].
    /// `ask` is the read's own, `None` until its first look makes it.
    pub fn read(&self, ask: &mut Option<Ask>, buf: &mut UserBytesMut) -> Option<u64> {
        let cpus = crate::arch::smp::cpu_count() as usize;
        let len = answer_len(cpus);
        if buf.len() < len {
            return Some(SyscallError::ResourceExhausted.to_u64());
        }
        let now = crate::clock::now();
        let ask = *ask.get_or_insert_with(|| Ask::issue(cpus, now));
        let answered = ask.answered();
        if !answered && !ask.deadline.reached(now) {
            return None;
        }
        if !answered {
            for cpu in (0..cpus).filter(|&cpu| !ASKS.served(cpu, ask.generation)) {
                crate::log!("perf_state: cpu{cpu} did not answer {:?} within {ANSWER}", ask.generation);
            }
            return Some(SyscallError::Io.to_u64());
        }
        let package = self.declared.as_ref().map_or_else(Default::default, perf_state::read_package);
        buf.write_at(0, package.as_bytes());
        // `answer_len(cpu)` is where CPU `cpu`'s record starts.
        for (cpu, slot) in SLOTS[..cpus].iter().enumerate() {
            buf.write_at(answer_len(cpu), slot.load().as_bytes());
        }
        Some(len as u64)
    }
}

/// Under `perf-state-deaf-cpu`, how many of the boot's asks only their asker
/// answers.
const DEAF_ASKS: u64 = 3;

/// The asks issued under `perf-state-deaf-cpu`.
static DEAF_ASKED: AtomicU64 = AtomicU64::new(0);

/// This CPU's answer, if one is owed. Called from `drain_irqs` every pass.
/// Under `perf-state-deaf-cpu` no CPU answers here until the boot's first
/// [`DEAF_ASKS`] asks are made, so each of those is answered by its asker alone.
pub fn serve_if_owed() {
    let me = crate::arch::percpu::cpu_id() as usize;
    let deaf = crate::actuator::perf_state_deaf_cpu() && DEAF_ASKED.load(Acquire) <= DEAF_ASKS;
    if !ASKS.owes(me) || deaf {
        return;
    }
    ASKS.serve(me, || SLOTS[me].store(sample(me)));
    WATCH.post();
}

/// This CPU's identity and registers, the registers zeros where the claim has
/// no declaration behind it.
fn sample(me: usize) -> CpuRegisters {
    // The proof is used inside the closure only: on an architecture where it
    // is uninhabited, binding one here would make the rest unreachable.
    let read = perf_state::declared().map(|declared| {
        move || {
            if crate::actuator::perf_request_diverges() && me == 1 {
                perf_state::diverge(&declared, 1);
            }
            perf_state::read_cpu(&declared)
        }
    });
    match read {
        Ok(read) => read(),
        Err(why) => {
            assert!(
                crate::actuator::perf_state_deaf_cpu(),
                "an ask is made only through a claim, and a claim only where the request is \
                 declared: {why}",
            );
            let hardware_id = u64::from(crate::arch::cpu::hardware_id());
            CpuRegisters { hardware_id, ..CpuRegisters::default() }
        }
    }
}
