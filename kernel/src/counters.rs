//! The machine's counters: per CPU, what [`Counter`] names, read by that CPU
//! into its own block and copied out by `SYS_COUNTERS`.
//!
//! **A CPU's hardware counters are readable only on that CPU**, so a read is a
//! round ([`Shootdown`]'s protocol): the reader issues a generation, answers
//! for its own CPU, kicks every other, and parks until each has answered or
//! the round's [`ANSWER`] bound has passed. A CPU answers from its kick
//! handler ([`serve_here`]) wherever it was; one whose interrupts stay closed
//! past the bound is reported stale with the last block it published, never
//! waited for without end. A read within [`JOIN`] of the last round's issue
//! waits on that round, so no reader makes a CPU take more than one kick per
//! [`JOIN`].
//!
//! A model-specific counter is read only where the CPU's own verdict at
//! [`bring_up`] admitted it, and the power envelope only where the CPU-state
//! declaration declared it; that bring-up's sample reads each once, so a wrong
//! verdict is `#GP` at boot, never inside a read.

use kernel::sched::task::WaitClass;
use toyos_abi::counters::{Counter, Record, RECORD_BYTES};
use toyos_abi::handle::Rights;
use toyos_abi::syscall::SyscallError;

use crate::arch::{irqchip, percpu, IrqGuard};
use crate::irq_census::Source;
use crate::scheduler::{Parkable, MAX_CPUS};
use crate::seqlock::Published;
use crate::shootdown::{Generation, Shootdown};
use crate::sync::Lock;
use crate::time::{Budget, Cadence, Deadline, Duration, Instant};
use crate::user_ptr::UserBytesMut;
use crate::watch::{self, IrqWatch};

/// How long a round waits for its answers.
const ANSWER: Budget = Budget::of(
    Duration::from_millis(100),
    "every CPU that has not answered is read stale, with the block it last published",
);

/// The most often a round is issued.
const JOIN: Cadence = Cadence::every(
    Duration::from_millis(10),
    "a reader within it of the last round's issue waits on that round, so a kick per CPU per 10 ms at most",
);

/// The counters a CPU's model-specific registers hold, where its bring-up
/// admitted them.
pub struct Hardware {
    pub smi: Option<u64>,
    pub aperf: Option<u64>,
    pub mperf: Option<u64>,
    pub envelope: Option<Envelope>,
    pub firmware: Option<Firmware>,
}

/// The commands this CPU wrote to the firmware, where it is the one that
/// writes them.
pub struct Firmware {
    pub calls: u64,
    pub nanos: u64,
}

/// The power envelope's registers, where the CPU's performance request is
/// declared.
pub struct Envelope {
    pub hwp_request: u64,
    pub hwp_request_pkg: u64,
    pub energy_perf_bias: u64,
}

/// A block's words: the hardware id, a bit per counter it holds, and a word
/// per counter.
const WORDS: usize = 2 + Counter::COUNT;

static ROUNDS: Shootdown = Shootdown::new();
static BLOCKS: [Published<WORDS>; MAX_CPUS] = [const { Published::new() }; MAX_CPUS];
/// Posted by every answer; a reader parks on it.
static ANSWERED: IrqWatch = IrqWatch::new();
/// The last round issued, and when.
static LAST: Lock<Option<(Generation, Instant)>> = Lock::new(None);

/// This CPU's verdict and its first block. Every CPU, once, before it is
/// asked anything.
pub fn bring_up() {
    crate::arch::counters::bring_up();
    let me = percpu::cpu_id() as usize;
    let words = sample(me);
    BLOCKS[me].publish(words);
    let [smi, aperf, mperf, request, pkg, epb] = [
        Counter::Smi,
        Counter::Aperf,
        Counter::Mperf,
        Counter::HwpRequest,
        Counter::HwpRequestPkg,
        Counter::EnergyPerfBias,
    ]
    .map(|c| held(&words, c));
    log!(
        "counters: cpu{me} reads smi={smi} aperf={aperf} mperf={mperf} hwp_request={request} \
         hwp_request_pkg={pkg} energy_perf_bias={epb}"
    );
}

fn held(words: &[u64; WORDS], counter: Counter) -> bool {
    words[1] & 1 << counter as usize != 0
}

/// This CPU's block, read on it.
fn sample(me: usize) -> [u64; WORDS] {
    let stamp = crate::arch::cpu::counter();
    let hardware = crate::arch::counters::read();
    let mut words = [0; WORDS];
    words[0] = u64::from(crate::arch::cpu::hardware_id());
    for counter in Counter::ALL {
        let value = match counter {
            Counter::Stamp => Some(stamp),
            Counter::Smi => hardware.smi,
            Counter::Aperf => hardware.aperf,
            Counter::Mperf => hardware.mperf,
            Counter::Kicks => crate::irq_census::deliveries(me as u32, Source::Kick),
            Counter::HwpRequest => hardware.envelope.as_ref().map(|e| e.hwp_request),
            Counter::HwpRequestPkg => hardware.envelope.as_ref().map(|e| e.hwp_request_pkg),
            Counter::EnergyPerfBias => hardware.envelope.as_ref().map(|e| e.energy_perf_bias),
            Counter::FirmwareCalls => hardware.firmware.as_ref().map(|f| f.calls),
            Counter::FirmwareNanos => hardware.firmware.as_ref().map(|f| f.nanos),
            Counter::Switches
            | Counter::Syscalls
            | Counter::IdleExits
            | Counter::IdleExitCycles
            | Counter::IrqCycles
            | Counter::InboxFires
            | Counter::InboxLooks
            | Counter::InboxRearms
            | Counter::PipeLockSpinCycles
            | Counter::PipeLockHoldCycles
            | Counter::PipeCopyBytes => Some(soft::get(me, counter)),
        };
        if let Some(value) = value {
            words[1] |= 1 << counter as usize;
            words[2 + counter as usize] = value;
        }
    }
    words
}

/// This CPU's answer to the round it owes, if it owes one: the kick handler's
/// on every architecture.
pub fn serve_here() {
    answer(false);
}

/// This CPU's answer, after kicking every other CPU when `kick` says so.
fn answer(kick: bool) {
    let served = {
        // One writer per block, and one CPU both skipped by the kick and answered for.
        let _closed = IrqGuard::close();
        if kick {
            irqchip::kick_all_but_self();
        }
        let me = percpu::cpu_id() as usize;
        #[cfg(feature = "test-actuators")]
        if deaf::DEAF.load(core::sync::atomic::Ordering::Relaxed) == me {
            return;
        }
        ROUNDS.serve_if_owed(me, || BLOCKS[me].publish(sample(me)))
    };
    if served {
        ANSWERED.post_in_place();
    }
}

/// One record per online CPU into `out`, or how many CPUs there are when `out`
/// is empty; `rights` is what the caller's capability holds, a counter answered
/// only where they contain what it needs.
pub fn read(rights: Rights, out: &mut UserBytesMut) -> Result<usize, SyscallError> {
    let cpus = crate::smp::cpu_count() as usize;
    if out.is_empty() {
        return Ok(cpus);
    }
    if out.len() < cpus * RECORD_BYTES {
        return Err(SyscallError::ResourceExhausted);
    }
    let now = crate::clock::now();
    let (generation, issued, fresh) = {
        let mut last = LAST.lock();
        match *last {
            Some((generation, issued)) if now < issued + JOIN.duration() => (generation, issued, false),
            _ => {
                let generation = ROUNDS.issue();
                *last = Some((generation, now));
                (generation, now, true)
            }
        }
    };
    // This CPU's kick, if it has one, waits behind this syscall.
    answer(fresh);
    let answered = || (0..cpus).all(|cpu| ROUNDS.served(cpu, generation));
    let deadline = Deadline::at(issued + ANSWER.duration());
    if watch::wait_until(&Parkable::at_entry(), &ANSWERED, 0, WaitClass::Other, deadline, answered).is_err() {
        return Err(SyscallError::Gone);
    }
    for cpu in 0..cpus {
        out.write_at(cpu * RECORD_BYTES, &record(cpu, generation, rights).encode().0);
    }
    Ok(cpus)
}

fn record(cpu: usize, generation: Generation, rights: Rights) -> Record {
    // Before the snapshot: a CPU read as answered is one whose block holds that answer or a later one.
    let fresh = ROUNDS.served(cpu, generation);
    let words = BLOCKS[cpu].snapshot();
    let mut values = [None; Counter::COUNT];
    if let Some(words) = &words {
        for counter in Counter::ALL.into_iter().filter(|&c| held(words, c) && rights.contains(c.needs())) {
            values[counter as usize] = Some(words[2 + counter as usize]);
        }
    }
    Record {
        cpu: cpu as u32,
        hardware_id: words.map_or(0, |w| w[0] as u32),
        stale: !fresh || words.is_none(),
        values,
    }
}

/// MEASUREMENT ONLY: the kernel's own work, counted per CPU by software.
pub mod soft {
    use core::sync::atomic::{AtomicU64, Ordering::Relaxed};

    use toyos_abi::counters::Counter;

    use crate::arch::percpu;
    use crate::scheduler::MAX_CPUS;

    const FIRST: usize = Counter::Switches as usize;
    const N: usize = Counter::COUNT - FIRST;

    #[repr(align(64))]
    struct Block {
        counts: [AtomicU64; N],
        /// The stamp the idle loop's last halt returned at, zero once spent.
        woke: AtomicU64,
    }

    static BLOCKS: [Block; MAX_CPUS] =
        [const { Block { counts: [const { AtomicU64::new(0) }; N], woke: AtomicU64::new(0) } }; MAX_CPUS];

    fn here() -> &'static Block {
        &BLOCKS[percpu::cpu_id() as usize]
    }

    #[inline]
    pub fn now() -> u64 {
        crate::arch::cpu::counter()
    }

    #[inline]
    pub fn add(counter: Counter, n: u64) {
        here().counts[counter as usize - FIRST].fetch_add(n, Relaxed);
    }

    /// Stamp ticks since `since`, added to `counter`.
    #[inline]
    pub fn since(counter: Counter, since: u64) {
        add(counter, now().wrapping_sub(since));
    }

    pub(super) fn get(cpu: usize, counter: Counter) -> u64 {
        BLOCKS[cpu].counts[counter as usize - FIRST].load(Relaxed)
    }

    /// The idle loop's halt returned.
    pub fn woke() {
        add(Counter::IdleExits, 1);
        here().woke.store(now(), Relaxed);
    }

    /// The CPU left the idle loop's wake for a switch or another halt.
    pub fn idle_left() {
        let woke = here().woke.swap(0, Relaxed);
        if woke != 0 {
            since(Counter::IdleExitCycles, woke);
        }
    }
}

/// `SYS_DEBUG`'s staging of a CPU that answers no round.
#[cfg(feature = "test-actuators")]
pub mod deaf {
    use core::sync::atomic::{AtomicUsize, Ordering::Relaxed};

    use toyos_abi::syscall::SyscallError;

    /// Outside every CPU index, so it names none.
    const NOBODY: usize = usize::MAX;

    pub(super) static DEAF: AtomicUsize = AtomicUsize::new(NOBODY);

    pub fn stage(cpu: u64) -> u64 {
        if cpu >= u64::from(crate::smp::cpu_count()) {
            return SyscallError::InvalidArgument.to_u64();
        }
        DEAF.store(cpu as usize, Relaxed);
        0
    }

    pub fn end() -> u64 {
        DEAF.store(NOBODY, Relaxed);
        0
    }
}
