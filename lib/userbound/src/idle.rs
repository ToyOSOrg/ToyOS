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
//! - a port is one the core itself traps as a C-state request and no device
//!   sees: `CStateBaseAddr`'s eight (AMD PPR 55922, MSRC001_0073);
//! - the CPU keeps time in any state: its APIC timer always runs (CPUID.06H:
//!   EAX[2]) and its TSC is invariant (CPUID.80000007H:EDX[8]). Without either a
//!   deep state stops the clock or the one-shot, so a CPU lacking one keeps
//!   only C1, whatever is declared.

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
    /// The first of the eight ports the core traps for C-state entry, where it
    /// traps any: `CStateBaseAddr` on AMD, nonzero.
    pub trapped_ports: Option<u16>,
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
    /// A port the core does not trap for C-state entry.
    Port { state: usize, port: u64 },
    /// A state that claims to wake in no time.
    NoLatency { state: usize },
}

/// `declaration` checked for the CPU `cpu` describes.
pub fn table(declaration: &IdleDeclaration, cpu: &CpuIdle) -> Result<Table, Refused> {
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
            IdleEntry::Port => {
                let trapped = cpu.trapped_ports.is_some_and(|base| (u64::from(base)..u64::from(base) + 8).contains(&declared.address));
                if !trapped {
                    return Err(Refused::Port { state, port: declared.address });
                }
                Entry::Port { port: declared.address as u16 }
            }
        };
        if declared.latency_us == 0 {
            return Err(Refused::NoLatency { state });
        }
        table.states[state] = Some(State {
            entry,
            exit_ns: u64::from(declared.latency_us) * 1000,
            residency_ns: u64::from(declared.residency_us) * 1000,
        });
    }
    Ok(table)
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
        trapped_ports: None,
    };

    /// A Zen 2's: MWAIT reaches C1 alone (PPR 55922, CPUID_Fn00000005_EDX
    /// 0000_0011h), and the core traps 0x413 to 0x41A.
    const ZEN_2: CpuIdle = CpuIdle {
        monitor: true,
        mwait_extensions: true,
        mwait_substates: 0x11,
        always_running_timer: true,
        invariant_tsc: true,
        trapped_ports: Some(0x413),
    };

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
        let t = table(&declaring(&[state(IdleEntry::Mwait, 0x31, 253), state(IdleEntry::Mwait, 0x60, 1048)]), &INTEL).unwrap();
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
            assert_eq!(table(&declaring(&[state(IdleEntry::Mwait, hint, 10)]), &INTEL), Err(Refused::Hint { state: 0, hint }), "{hint:#x}");
        }
        assert_eq!(table(&declaring(&[state(IdleEntry::Mwait, 0x31, 10)]), &ZEN_2), Err(Refused::Hint { state: 0, hint: 0x31 }));
    }

    #[test]
    fn the_amds_trapped_port_is_kept_and_any_other_refused() {
        let kept = table(&declaring(&[state(IdleEntry::Port, 0x414, 400)]), &ZEN_2).unwrap();
        assert_eq!(kept.states().next().unwrap().entry, Entry::Port { port: 0x414 });
        for port in [0x412, 0x41B, 0x1816, 0x1_0414] {
            assert_eq!(table(&declaring(&[state(IdleEntry::Port, port, 400)]), &ZEN_2), Err(Refused::Port { state: 0, port }), "{port:#x}");
        }
        assert_eq!(table(&declaring(&[state(IdleEntry::Port, 0x414, 400)]), &INTEL), Err(Refused::Port { state: 0, port: 0x414 }));
    }

    #[test]
    fn a_cpu_whose_timer_or_clock_stops_keeps_c1_alone() {
        let d = declaring(&[state(IdleEntry::Mwait, 0x31, 253)]);
        assert_eq!(table(&d, &CpuIdle { always_running_timer: false, ..INTEL }), Err(Refused::TimerStops));
        assert_eq!(table(&d, &CpuIdle { invariant_tsc: false, ..INTEL }), Err(Refused::TscVaries));
        assert_eq!(table(&declaring(&[]), &CpuIdle::default()), Ok(Table::EMPTY), "nothing declared needs nothing of the CPU");
    }

    #[test]
    fn a_malformed_declaration_is_refused_whole() {
        let mut d = declaring(&[state(IdleEntry::Mwait, 0x31, 253)]);
        d.count = 9;
        assert_eq!(table(&d, &INTEL), Err(Refused::Count(9)));
        let mut bad = state(IdleEntry::Mwait, 0x31, 253);
        bad.entry = 3;
        assert_eq!(table(&declaring(&[bad]), &INTEL), Err(Refused::Malformed { state: 0 }));
        let mut bad = state(IdleEntry::Mwait, 0x31, 253);
        bad.reserved[6] = 1;
        assert_eq!(table(&declaring(&[bad]), &INTEL), Err(Refused::Malformed { state: 0 }));
        assert_eq!(table(&declaring(&[state(IdleEntry::Mwait, 0x31, 0)]), &INTEL), Err(Refused::NoLatency { state: 0 }));
        assert_eq!(table(&declaring(&[state(IdleEntry::Mwait, 0x31, 1)]), &CpuIdle { mwait_extensions: false, ..INTEL }), Err(Refused::NoMwait { state: 0 }));
    }
}
