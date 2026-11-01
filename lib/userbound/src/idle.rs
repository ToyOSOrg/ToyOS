//! A processor's idle states as the `acpi` claim's holder declared them
//! (`toyos_abi::acpi::op::IDLE`), checked against the CPU that will enter
//! them before the kernel keeps any.
//!
//! **The holder is trusted platform code, and these are bug fences**: what a
//! state means is the firmware's and the holder's, and the holder hands over
//! only states that need nothing around their entry. What is checked here is
//! that the CPU can execute the entry and that it touches nothing:
//!
//! - an `MWAIT` hint names a sub-state CPUID leaf 5 enumerates, on a CPU with
//!   MONITOR/MWAIT and its extensions (Intel SDM Vol. 2B, MWAIT, Table 1-3);
//! - a port is one the holder could read itself through its claim: no port
//!   the kernel keeps, protects or lent to another claim
//!   (`crate::firmware::port`, asked by the caller). The port comes from the
//!   firmware's AML, which is trusted platform code, and a read the kernel
//!   makes for the holder is one the holder's AML could make, so this fence
//!   keeps the read off what the kernel owns and needs no list of which parts
//!   trap which ports. On a part where the port is no trapped C-state request
//!   the read returns at once and the CPU passes back through selection: a
//!   short unforeseen period the history learns, never a hang;
//! - the CPU keeps time in any state: its APIC timer always runs (CPUID.06H:
//!   EAX[2]) and its TSC is invariant (CPUID.80000007H:EDX[8]). Without either a
//!   deep state stops the clock or the one-shot, so a CPU lacking one keeps
//!   only C1, whatever is declared;
//! - a state's residency is no shorter than its exit: the CPU wakes `exit`
//!   early before a deadline (`kernel::sched::idle`), which is a wait only
//!   where the state fits a residency longer than that.
//!
//! The kernel keeps a CPU's table as words it publishes and its CPU reads
//! ([`words`], [`read`]).

use toyos_abi::acpi::{IdleDeclaration, IdleEntry, IDLE_STATES};

/// What decides whether one CPU can enter a declared state, read on that CPU.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct CpuIdle {
    /// CPUID.01H:ECX[3].
    pub monitor: bool,
    /// CPUID.05H:ECX[0], MWAIT's extensions enumerated.
    pub mwait_extensions: bool,
    /// CPUID.05H:EDX: four bits of sub-state count per C-state, C0 lowest.
    pub mwait_substates: u32,
    /// CPUID.06H:EAX[2].
    pub always_running_timer: bool,
    /// CPUID.80000007H:EDX[8].
    pub invariant_tsc: bool,
}

/// How a checked state is entered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Entry {
    Mwait { hint: u32 },
    Port { port: u16 },
}

/// One checked state, its costs in nanoseconds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct State {
    pub entry: Entry,
    pub exit_ns: u64,
    pub residency_ns: u64,
}

/// A CPU's checked states, shallowest first.
///
/// ```compile_fail,E0451
/// let _ = toyos_userbound::idle::Table { states: [None; 8] };
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Table {
    states: [Option<State>; IDLE_STATES],
}

impl Table {
    /// No state but C1.
    pub const EMPTY: Self = Self { states: [None; IDLE_STATES] };

    pub fn states(&self) -> impl Iterator<Item = &State> {
        self.states.iter().map_while(Option::as_ref)
    }

    pub fn len(&self) -> usize {
        self.states().count()
    }

    pub fn is_empty(&self) -> bool {
        self.states[0].is_none()
    }
}

/// Why a declaration was not kept; `state` is its index in the declaration.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refused {
    /// More states than [`IDLE_STATES`].
    Count(u32),
    /// The CPU's APIC timer stops in a deep state.
    TimerStops,
    /// The CPU's TSC is not invariant.
    TscVaries,
    /// An entry this ABI does not name, or a reserved byte set.
    Malformed { state: usize },
    /// An `MWAIT` state on a CPU without MONITOR/MWAIT and its extensions.
    NoMwait { state: usize },
    /// A hint with reserved bits, or a sub-state leaf 5 does not enumerate.
    Hint { state: usize, hint: u64 },
    /// A port past 0xFFFF, or one the holder could not read itself.
    Port { state: usize, port: u64 },
    /// A state that claims to wake in no time.
    NoLatency { state: usize },
    /// A residency shorter than the state's exit, zero among them.
    Residency { state: usize },
}

/// `declaration` checked for the CPU `cpu` describes; `readable` says whether
/// the holder could read a port itself.
pub fn table(declaration: &IdleDeclaration, cpu: &CpuIdle, readable: impl Fn(u16) -> bool) -> Result<Table, Refused> {
    let count = declaration.count as usize;
    if count > IDLE_STATES {
        return Err(Refused::Count(declaration.count));
    }
    let mut table = Table::EMPTY;
    if count == 0 {
        return Ok(table);
    }
    if !cpu.always_running_timer {
        return Err(Refused::TimerStops);
    }
    if !cpu.invariant_tsc {
        return Err(Refused::TscVaries);
    }
    for (state, declared) in declaration.states[..count].iter().enumerate() {
        if declared.reserved != [0; 7] {
            return Err(Refused::Malformed { state });
        }
        let entry = match IdleEntry::from_raw(declared.entry).ok_or(Refused::Malformed { state })? {
            IdleEntry::Mwait => {
                if !cpu.monitor || !cpu.mwait_extensions {
                    return Err(Refused::NoMwait { state });
                }
                Entry::Mwait { hint: mwait_hint(declared.address, cpu.mwait_substates).ok_or(Refused::Hint { state, hint: declared.address })? }
            }
            IdleEntry::Port => match u16::try_from(declared.address) {
                Ok(port) if readable(port) => Entry::Port { port },
                _ => return Err(Refused::Port { state, port: declared.address }),
            },
        };
        if declared.latency_us == 0 {
            return Err(Refused::NoLatency { state });
        }
        if declared.residency_us < declared.latency_us {
            return Err(Refused::Residency { state });
        }
        table.states[state] = Some(State {
            entry,
            exit_ns: u64::from(declared.latency_us) * 1000,
            residency_ns: u64::from(declared.residency_us) * 1000,
        });
    }
    Ok(table)
}

/// A table's words: a generation and the count, then two per state.
pub const WORDS: usize = 1 + 2 * IDLE_STATES;

const MWAIT: u64 = 1;
const PORT: u64 = 2;

/// `table` as words, for one publisher to store and its CPU to [`read`]: the
/// generation tells this table from the one it replaces.
pub fn words(table: &Table, generation: u64) -> [u64; WORDS] {
    let mut words = [0; WORDS];
    words[0] = (generation << 8) | table.len() as u64;
    for (i, state) in table.states().enumerate() {
        let (kind, address) = match state.entry {
            Entry::Mwait { hint } => (MWAIT, u64::from(hint)),
            Entry::Port { port } => (PORT, u64::from(port)),
        };
        words[1 + 2 * i] = kind | (address << 8);
        // Microseconds, as declared: each fits its half.
        words[2 + 2 * i] = (state.exit_ns / 1000) | ((state.residency_ns / 1000) << 32);
    }
    words
}

/// What [`words`] wrote: the generation, and the states in order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Published {
    pub generation: u64,
    pub states: [Option<State>; IDLE_STATES],
}

/// The words [`words`] wrote, read back. Words it did not write are a
/// kernel bug: the one publisher writes nothing else.
pub fn read(words: &[u64; WORDS]) -> Published {
    let len = (words[0] & 0xFF) as usize;
    assert!(len <= IDLE_STATES, "idle: a published table of {len} states, which no table holds");
    let mut states = [None; IDLE_STATES];
    for (i, slot) in states.iter_mut().enumerate().take(len) {
        let (head, cost) = (words[1 + 2 * i], words[2 + 2 * i]);
        let entry = match head & 0xFF {
            MWAIT => Entry::Mwait { hint: (head >> 8) as u32 },
            PORT => Entry::Port { port: (head >> 8) as u16 },
            kind => panic!("idle: a published state of kind {kind}, which words writes none of"),
        };
        *slot = Some(State { entry, exit_ns: (cost & 0xFFFF_FFFF) * 1000, residency_ns: (cost >> 32) * 1000 });
    }
    Published { generation: words[0] >> 8, states }
}

/// `hint` where leaf 5 enumerates its sub-state: bits 7:4 the target C-state
/// less one (15 is C0), bits 3:0 the sub-state, the rest reserved (SDM Table
/// 1-3); `substates` holds C0's count in bits 3:0, C1's in 7:4, and so on.
fn mwait_hint(hint: u64, substates: u32) -> Option<u32> {
    if hint > 0xFF {
        return None;
    }
    let (target, sub) = ((hint >> 4) as u32, (hint & 0xF) as u32);
    let nibble = target + 1;
    if nibble >= 8 {
        return None;
    }
    (sub < substates >> (4 * nibble) & 0xF).then_some(hint as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use toyos_abi::acpi::IdleState;

    /// An Intel CPU whose leaf 5 counts sub-states C0 0, C1 2, C2 1, C3 2,
    /// C4 2, C5 0, C6 0, C7 1: hint 0x31 (C4's second) and 0x60 (C7's first),
    /// the T14's `_CST` hints, exist on it.
    const INTEL: CpuIdle = CpuIdle {
        monitor: true,
        mwait_extensions: true,
        mwait_substates: 0x1002_2120,
        always_running_timer: true,
        invariant_tsc: true,
    };

    /// A Zen 2's: MWAIT reaches C1 alone (PPR 55922, CPUID_Fn00000005_EDX
    /// 0000_0011h).
    const ZEN_2: CpuIdle = CpuIdle {
        monitor: true,
        mwait_extensions: true,
        mwait_substates: 0x11,
        always_running_timer: true,
        invariant_tsc: true,
    };

    /// The holder's port policy as a test asks it: every port but the ones
    /// the kernel keeps, here the 8259 pair's.
    fn readable(port: u16) -> bool {
        !matches!(port, 0x20 | 0x21 | 0xA0 | 0xA1)
    }

    fn state(entry: IdleEntry, address: u64, latency_us: u32) -> IdleState {
        IdleState { address, latency_us, residency_us: 2 * latency_us, entry: entry as u8, reserved: [0; 7] }
    }

    fn declaring(states: &[IdleState]) -> IdleDeclaration {
        let mut d = IdleDeclaration { processor_uid: 0, count: states.len() as u32, states: [state(IdleEntry::Mwait, 0, 0); IDLE_STATES] };
        d.states[..states.len()].copy_from_slice(states);
        d
    }

    #[test]
    fn the_t14s_two_states_are_kept_as_declared() {
        let t = table(&declaring(&[state(IdleEntry::Mwait, 0x31, 253), state(IdleEntry::Mwait, 0x60, 1048)]), &INTEL, readable).unwrap();
        let mut states = [None; 2];
        for (slot, kept) in states.iter_mut().zip(t.states()) {
            *slot = Some(*kept);
        }
        assert_eq!(t.len(), 2);
        assert_eq!(
            states.map(Option::unwrap),
            [
                State { entry: Entry::Mwait { hint: 0x31 }, exit_ns: 253_000, residency_ns: 506_000 },
                State { entry: Entry::Mwait { hint: 0x60 }, exit_ns: 1_048_000, residency_ns: 2_096_000 },
            ]
        );
    }

    #[test]
    fn a_hint_leaf_5_does_not_enumerate_is_refused() {
        // C3 has two sub-states, so 0x22 names a third; C6 has none; a target
        // of 7 would be C8, past leaf 5; 0xF0 is C0; 0x100 sets a reserved bit.
        for hint in [0x22, 0x50, 0x70, 0xF0, 0x100] {
            assert_eq!(table(&declaring(&[state(IdleEntry::Mwait, hint, 10)]), &INTEL, readable), Err(Refused::Hint { state: 0, hint }), "{hint:#x}");
        }
        assert_eq!(table(&declaring(&[state(IdleEntry::Mwait, 0x31, 10)]), &ZEN_2, readable), Err(Refused::Hint { state: 0, hint: 0x31 }));
    }

    #[test]
    fn a_port_the_holder_may_read_is_kept_and_any_other_refused() {
        let kept = table(&declaring(&[state(IdleEntry::Port, 0x414, 400)]), &ZEN_2, readable).unwrap();
        assert_eq!(kept.states().next().unwrap().entry, Entry::Port { port: 0x414 });
        for port in [0x20, 0xA1, 0x1_0414] {
            assert_eq!(table(&declaring(&[state(IdleEntry::Port, port, 400)]), &ZEN_2, readable), Err(Refused::Port { state: 0, port }), "{port:#x}");
        }
    }

    /// The early wake before a deadline is the exit; a state whose residency
    /// is shorter would be woken from before it was worth entering.
    #[test]
    fn a_residency_shorter_than_the_exit_is_refused() {
        let with = |latency_us, residency_us| IdleState { residency_us, ..state(IdleEntry::Mwait, 0x31, latency_us) };
        assert_eq!(table(&declaring(&[with(253, 252)]), &INTEL, readable), Err(Refused::Residency { state: 0 }));
        assert_eq!(table(&declaring(&[with(253, 0)]), &INTEL, readable), Err(Refused::Residency { state: 0 }));
        assert!(table(&declaring(&[with(253, 253)]), &INTEL, readable).is_ok());
    }

    #[test]
    fn a_table_reads_back_from_its_words_whole() {
        let t = table(&declaring(&[state(IdleEntry::Mwait, 0x31, 253), state(IdleEntry::Port, 0x414, 400)]), &INTEL, readable);
        // Ports are no Intel state, but the boundary checks only what the CPU can do: a port is read on any.
        let t = t.unwrap();
        let back = read(&words(&t, 7));
        assert_eq!(back.generation, 7);
        let mut expected = [None; IDLE_STATES];
        for (slot, kept) in expected.iter_mut().zip(t.states()) {
            *slot = Some(*kept);
        }
        assert_eq!(back.states, expected);
        assert_eq!(read(&words(&Table::EMPTY, 9)), Published { generation: 9, states: [None; IDLE_STATES] });
    }

    #[test]
    fn a_cpu_whose_timer_or_clock_stops_keeps_c1_alone() {
        let d = declaring(&[state(IdleEntry::Mwait, 0x31, 253)]);
        assert_eq!(table(&d, &CpuIdle { always_running_timer: false, ..INTEL }, readable), Err(Refused::TimerStops));
        assert_eq!(table(&d, &CpuIdle { invariant_tsc: false, ..INTEL }, readable), Err(Refused::TscVaries));
        assert_eq!(table(&declaring(&[]), &CpuIdle::default(), readable), Ok(Table::EMPTY), "nothing declared needs nothing of the CPU");
    }

    #[test]
    fn a_malformed_declaration_is_refused_whole() {
        let mut d = declaring(&[state(IdleEntry::Mwait, 0x31, 253)]);
        d.count = 9;
        assert_eq!(table(&d, &INTEL, readable), Err(Refused::Count(9)));
        let mut bad = state(IdleEntry::Mwait, 0x31, 253);
        bad.entry = 3;
        assert_eq!(table(&declaring(&[bad]), &INTEL, readable), Err(Refused::Malformed { state: 0 }));
        let mut bad = state(IdleEntry::Mwait, 0x31, 253);
        bad.reserved[6] = 1;
        assert_eq!(table(&declaring(&[bad]), &INTEL, readable), Err(Refused::Malformed { state: 0 }));
        assert_eq!(table(&declaring(&[state(IdleEntry::Mwait, 0x31, 0)]), &INTEL, readable), Err(Refused::NoLatency { state: 0 }));
        assert_eq!(table(&declaring(&[state(IdleEntry::Mwait, 0x31, 1)]), &CpuIdle { mwait_extensions: false, ..INTEL }, readable), Err(Refused::NoMwait { state: 0 }));
    }
}
