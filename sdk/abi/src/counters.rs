//! What [`SYS_COUNTERS`] answers: one [`Record`] per online CPU, each holding
//! that CPU's [`Counter`]s as it read them itself.
//!
//! **A CPU's hardware counters are readable only on that CPU**, so a read asks
//! every CPU to copy its counters out and waits for the answers under a bound.
//! A CPU that has not answered by then is [`Record::stale`]: its values are the
//! last it gave, and its [`Counter::Stamp`] says when it gave them. A counter
//! the CPU does not have, or that the caller's capability may not read, is
//! absent and never zero; a record with no stamp is a CPU whose answer could
//! not be read whole, and says nothing about it but its index.
//!
//! Every counter counts up from boot, so the difference of two reads is exact;
//! the power envelope's registers ([`Counter::HwpRequest`],
//! [`Counter::HwpRequestPkg`] and [`Counter::EnergyPerfBias`]) count nothing
//! and are what the CPU held when it read them.
//!
//! [`SYS_COUNTERS`]: crate::syscall::SYS_COUNTERS

use crate::handle::Rights;

/// One counter a CPU keeps, by its index in [`Record::values`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Counter {
    /// The CPU's own free-running counter (TSC, `CNTVCT_EL0`) when it read the
    /// rest, in its ticks.
    Stamp,
    /// Times firmware took the CPU over (`MSR_SMI_COUNT`), 32 bits wide.
    Smi,
    /// Cycles at the frequency the CPU ran at, counted while it ran
    /// (`IA32_APERF`).
    Aperf,
    /// Cycles at a fixed reference frequency, counted while it ran
    /// (`IA32_MPERF`).
    Mperf,
    /// Kicks the CPU took: the interrupt another CPU sends it to wake it.
    Kicks,
    /// The CPU's performance request (`IA32_HWP_REQUEST`), where the kernel
    /// declares one.
    HwpRequest,
    /// Its package's request (`IA32_HWP_REQUEST_PKG`), beside it.
    HwpRequestPkg,
    /// Its energy/performance bias (`IA32_ENERGY_PERF_BIAS`), beside it.
    EnergyPerfBias,
    /// Commands the kernel wrote to the firmware on this CPU (the FADT's
    /// `SMI_CMD`), whoever asked: its own to enter and leave ACPI mode, and
    /// each call it made for the `acpi` claim's holder. Held by the CPU that
    /// writes them, the boot processor, on a machine that names the port.
    FirmwareCalls,
    /// Nanoseconds those writes held that CPU, each from before the write to
    /// its return: the firmware's handler, where a write raises an interrupt
    /// to it.
    FirmwareNanos,
    /// MEASUREMENT ONLY, the kernel's own work on this CPU, counted by software:
    /// context switches onto any context, idle included.
    Switches,
    /// Syscalls entered.
    Syscalls,
    /// Halts the idle loop came back from.
    IdleExits,
    /// Stamp ticks from each halt's return to the next switch or halt.
    IdleExitCycles,
    /// Stamp ticks inside the timer, kick and claimed-device handlers.
    IrqCycles,
    /// Posts that reached an inbox poll.
    InboxFires,
    /// Looks an inbox's submitter took at a fired poll.
    InboxLooks,
    /// Polls a look found nothing for and armed again.
    InboxRearms,
    /// Stamp ticks spent taking the pipe table's lock.
    PipeLockSpinCycles,
    /// Stamp ticks the pipe table's lock was held.
    PipeLockHoldCycles,
    /// Bytes pipe reads and writes copied.
    PipeCopyBytes,
    /// MEASUREMENT ONLY: instructions this CPU retired, both rings
    /// (`IA32_FIXED_CTR0`).
    Instructions,
    /// MEASUREMENT ONLY: its package's energy (`MSR_PKG_ENERGY_STATUS`), in
    /// microjoules, carried past the register's 32-bit wrap.
    PkgEnergyUj,
    /// MEASUREMENT ONLY, the clock scout: core C6 residency (`MSR_CORE_C6_RESIDENCY`,
    /// 0x3FD), in TSC ticks, the core's, so both its threads read the same.
    CoreC6,
    /// Core C7 residency (0x3FE).
    CoreC7,
    /// Package C2 residency (`MSR_PKG_C2_RESIDENCY`, 0x60D), in TSC ticks.
    PkgC2,
    /// Package C3 residency (0x3F8).
    PkgC3,
    /// Package C6 residency (0x3F9).
    PkgC6,
    /// Package C7 residency (0x3FA).
    PkgC7,
    /// Package C8 residency (0x630).
    PkgC8,
    /// Package C9 residency (0x631).
    PkgC9,
    /// Package C10 residency (0x632).
    PkgC10,
    /// `IA32_PERF_STATUS` (0x198) as read: bits 15:8 the ratio the core runs at.
    PerfStatus,
    /// `IA32_HWP_STATUS` (0x777): the log bits set since this CPU's previous
    /// read, which clears them.
    HwpStatus,
    /// `MSR_CORE_PERF_LIMIT_REASONS` (0x64F): bits 15:0 what limits the clock
    /// now, bits 31:16 what did since the previous read on its package, which
    /// clears them.
    PerfLimitReasons,
    /// `IA32_HWP_CAPABILITIES` (0x771) as read.
    HwpCapabilities,
    /// `MSR_PKG_POWER_LIMIT` (0x610) as read: PL1 in bits 14:0, PL2 in 46:32,
    /// in [`Counter::RaplPowerUnit`]'s units.
    PkgPowerLimit,
    /// `MSR_RAPL_POWER_UNIT` (0x606) as read.
    RaplPowerUnit,
    /// `IA32_PERF_GLOBAL_CTRL` (0x38F) as read: which counters count.
    PmuGlobalCtrl,
    /// `MSR_POWER_CTL` (0x1FC) as read: bit 1 promotes C1 to C1E.
    PowerCtl,
    /// `MSR_PKG_CST_CONFIG_CONTROL` (0xE2) as read: bits 3:0 the deepest
    /// package C-state firmware allows.
    PkgCstConfig,
    /// MEASUREMENT ONLY, the contention scout: retired loads whose data came
    /// from another core's modified line (`MEM_LOAD_L3_HIT_RETIRED.XSNP_HITM`,
    /// event D2H umask 04H), both rings.
    HitmLoads,
    /// Demand RFOs answered by a snoop that hit another core's modified line
    /// (`OCR.DEMAND_RFO.L3_HIT.SNOOP_HITM`, event B7H umask 01H,
    /// `MSR_OFFCORE_RSP_0` 0x10003C0002), both rings.
    HitmRfos,
    /// Takes of the PMM's `BITMAP` lock.
    PmmLockTakes,
    /// Of them, a user copy's frame pin or unpin.
    PmmPinTakes,
    /// Stamp ticks spent taking it.
    PmmLockSpinCycles,
    /// Stamp ticks it was held.
    PmmLockHoldCycles,
}

impl Counter {
    pub const COUNT: usize = 47;
    pub const ALL: [Counter; Self::COUNT] = [
        Self::Stamp,
        Self::Smi,
        Self::Aperf,
        Self::Mperf,
        Self::Kicks,
        Self::HwpRequest,
        Self::HwpRequestPkg,
        Self::EnergyPerfBias,
        Self::FirmwareCalls,
        Self::FirmwareNanos,
        Self::Switches,
        Self::Syscalls,
        Self::IdleExits,
        Self::IdleExitCycles,
        Self::IrqCycles,
        Self::InboxFires,
        Self::InboxLooks,
        Self::InboxRearms,
        Self::PipeLockSpinCycles,
        Self::PipeLockHoldCycles,
        Self::PipeCopyBytes,
        Self::Instructions,
        Self::PkgEnergyUj,
        Self::CoreC6,
        Self::CoreC7,
        Self::PkgC2,
        Self::PkgC3,
        Self::PkgC6,
        Self::PkgC7,
        Self::PkgC8,
        Self::PkgC9,
        Self::PkgC10,
        Self::PerfStatus,
        Self::HwpStatus,
        Self::PerfLimitReasons,
        Self::HwpCapabilities,
        Self::PkgPowerLimit,
        Self::RaplPowerUnit,
        Self::PmuGlobalCtrl,
        Self::PowerCtl,
        Self::PkgCstConfig,
        Self::HitmLoads,
        Self::HitmRfos,
        Self::PmmLockTakes,
        Self::PmmPinTakes,
        Self::PmmLockSpinCycles,
        Self::PmmLockHoldCycles,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Stamp => "stamp",
            Self::Smi => "smi",
            Self::Aperf => "aperf",
            Self::Mperf => "mperf",
            Self::Kicks => "kicks",
            Self::HwpRequest => "hwp_request",
            Self::HwpRequestPkg => "hwp_request_pkg",
            Self::EnergyPerfBias => "energy_perf_bias",
            Self::FirmwareCalls => "firmware_calls",
            Self::FirmwareNanos => "firmware_nanos",
            Self::Switches => "switches",
            Self::Syscalls => "syscalls",
            Self::IdleExits => "idle_exits",
            Self::IdleExitCycles => "idle_exit_cycles",
            Self::IrqCycles => "irq_cycles",
            Self::InboxFires => "inbox_fires",
            Self::InboxLooks => "inbox_looks",
            Self::InboxRearms => "inbox_rearms",
            Self::PipeLockSpinCycles => "pipe_lock_spin_cycles",
            Self::PipeLockHoldCycles => "pipe_lock_hold_cycles",
            Self::PipeCopyBytes => "pipe_copy_bytes",
            Self::Instructions => "instructions",
            Self::PkgEnergyUj => "pkg_energy_uj",
            Self::CoreC6 => "core_c6",
            Self::CoreC7 => "core_c7",
            Self::PkgC2 => "pkg_c2",
            Self::PkgC3 => "pkg_c3",
            Self::PkgC6 => "pkg_c6",
            Self::PkgC7 => "pkg_c7",
            Self::PkgC8 => "pkg_c8",
            Self::PkgC9 => "pkg_c9",
            Self::PkgC10 => "pkg_c10",
            Self::PerfStatus => "perf_status",
            Self::HwpStatus => "hwp_status",
            Self::PerfLimitReasons => "perf_limit_reasons",
            Self::HwpCapabilities => "hwp_capabilities",
            Self::PkgPowerLimit => "pkg_power_limit",
            Self::RaplPowerUnit => "rapl_power_unit",
            Self::PmuGlobalCtrl => "pmu_global_ctrl",
            Self::PowerCtl => "power_ctl",
            Self::PkgCstConfig => "pkg_cst_config",
            Self::HitmLoads => "hitm_loads",
            Self::HitmRfos => "hitm_rfos",
            Self::PmmLockTakes => "pmm_lock_takes",
            Self::PmmPinTakes => "pmm_pin_takes",
            Self::PmmLockSpinCycles => "pmm_lock_spin_cycles",
            Self::PmmLockHoldCycles => "pmm_lock_hold_cycles",
        }
    }

    /// The rights a `SysCap` carries for this counter to be answered.
    pub const fn needs(self) -> Rights {
        match self {
            Self::Stamp | Self::Smi | Self::FirmwareCalls | Self::FirmwareNanos => Rights::COUNTERS,
            Self::Aperf
            | Self::Mperf
            | Self::Kicks
            | Self::HwpRequest
            | Self::HwpRequestPkg
            | Self::EnergyPerfBias
            | Self::Switches
            | Self::Syscalls
            | Self::IdleExits
            | Self::IdleExitCycles
            | Self::IrqCycles
            | Self::InboxFires
            | Self::InboxLooks
            | Self::InboxRearms
            | Self::PipeLockSpinCycles
            | Self::PipeLockHoldCycles
            | Self::PipeCopyBytes
            | Self::Instructions
            | Self::PkgEnergyUj
            | Self::CoreC6
            | Self::CoreC7
            | Self::PkgC2
            | Self::PkgC3
            | Self::PkgC6
            | Self::PkgC7
            | Self::PkgC8
            | Self::PkgC9
            | Self::PkgC10
            | Self::PerfStatus
            | Self::HwpStatus
            | Self::PerfLimitReasons
            | Self::HwpCapabilities
            | Self::PkgPowerLimit
            | Self::RaplPowerUnit
            | Self::PmuGlobalCtrl
            | Self::PowerCtl
            | Self::PkgCstConfig
            | Self::HitmLoads
            | Self::HitmRfos
            | Self::PmmLockTakes
            | Self::PmmPinTakes
            | Self::PmmLockSpinCycles
            | Self::PmmLockHoldCycles => Rights::COUNTERS.union(Rights::TRACE),
        }
    }
}

/// The width of every record: the CPU's index and hardware id, the flags, and
/// one word per counter.
pub const RECORD_BYTES: usize = 16 + 8 * Counter::COUNT;

/// One record as it crosses the boundary.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RawRecord(pub [u8; RECORD_BYTES]);

impl RawRecord {
    pub const EMPTY: Self = Self([0; RECORD_BYTES]);
}

const STALE: u64 = 1;

const fn present(counter: Counter) -> u64 {
    1 << (8 + counter as u32)
}

/// One CPU's counters.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Record {
    pub cpu: u32,
    /// The id the CPU read off itself (its local APIC id, its packed MPIDR
    /// affinity), meaningful where the stamp is present.
    pub hardware_id: u32,
    /// The CPU did not answer this read within its bound.
    pub stale: bool,
    pub values: [Option<u64>; Counter::COUNT],
}

/// A record whose bytes no kernel writes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Undecodable {
    /// A flag bit no counter names.
    Flags(u64),
    /// A counter absent and its word not zero.
    Absent(Counter),
}

impl Record {
    pub fn get(&self, counter: Counter) -> Option<u64> {
        self.values[counter as usize]
    }

    pub fn encode(&self) -> RawRecord {
        let mut raw = [0u8; RECORD_BYTES];
        let mut flags = if self.stale { STALE } else { 0 };
        for counter in Counter::ALL {
            if let Some(value) = self.get(counter) {
                flags |= present(counter);
                let at = 16 + 8 * counter as usize;
                raw[at..at + 8].copy_from_slice(&value.to_le_bytes());
            }
        }
        raw[0..4].copy_from_slice(&self.cpu.to_le_bytes());
        raw[4..8].copy_from_slice(&self.hardware_id.to_le_bytes());
        raw[8..16].copy_from_slice(&flags.to_le_bytes());
        RawRecord(raw)
    }

    pub fn decode(raw: &RawRecord) -> Result<Self, Undecodable> {
        let word = |at: usize| u64::from_le_bytes(raw.0[at..at + 8].try_into().unwrap());
        let flags = word(8);
        let known = Counter::ALL.iter().fold(STALE, |known, &c| known | present(c));
        if flags & !known != 0 {
            return Err(Undecodable::Flags(flags & !known));
        }
        let mut values = [None; Counter::COUNT];
        for counter in Counter::ALL {
            let value = word(16 + 8 * counter as usize);
            if flags & present(counter) != 0 {
                values[counter as usize] = Some(value);
            } else if value != 0 {
                return Err(Undecodable::Absent(counter));
            }
        }
        Ok(Self {
            cpu: u32::from_le_bytes(raw.0[0..4].try_into().unwrap()),
            hardware_id: u32::from_le_bytes(raw.0[4..8].try_into().unwrap()),
            stale: flags & STALE != 0,
            values,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(stale: bool, first: [Option<u64>; 10]) -> Record {
        let mut values = [None; Counter::COUNT];
        values[..10].copy_from_slice(&first);
        Record { cpu: 7, hardware_id: 0x0102_0304, stale, values }
    }

    #[test]
    fn every_shape_of_record_reads_back_as_written() {
        for r in [
            record(false, [Some(1), Some(u64::from(u32::MAX)), Some(3), Some(u64::MAX), Some(0), Some(0x8000_2a04), Some(6), Some(0), Some(2), Some(31_936)]),
            record(true, [Some(9), None, None, None, Some(4), None, Some(0x8000_ff01), None, Some(0), None]),
            record(true, [None; 10]),
        ] {
            assert_eq!(Record::decode(&r.encode()), Ok(r));
        }
    }

    /// Absent is not zero: a zero that is there reads back there.
    #[test]
    fn a_present_zero_is_not_an_absent_counter() {
        let r = record(false, [Some(5), Some(0), None, None, None, None, None, None, None, None]);
        assert_eq!(Record::decode(&r.encode()).unwrap().get(Counter::Smi), Some(0));
        assert_eq!(Record::decode(&r.encode()).unwrap().get(Counter::Aperf), None);
    }

    #[test]
    fn a_flag_no_counter_names_is_refused() {
        let mut raw = record(false, [Some(1); 10]).encode();
        raw.0[8] |= 1 << 1;
        assert_eq!(Record::decode(&raw), Err(Undecodable::Flags(1 << 1)));
        let mut raw = RawRecord::EMPTY;
        raw.0[15] = 0x80;
        assert_eq!(Record::decode(&raw), Err(Undecodable::Flags(1 << 63)));
    }

    #[test]
    fn a_word_under_an_absent_counter_is_refused() {
        let mut raw = record(false, [Some(1), None, None, None, None, None, None, None, None, None]).encode();
        raw.0[16 + 8 * Counter::Mperf as usize] = 1;
        assert_eq!(Record::decode(&raw), Err(Undecodable::Absent(Counter::Mperf)));
    }

    /// The counters that time programs and the power envelope are the trace
    /// right's, and the four that are neither, the CPU's stamp and what its
    /// firmware took of it, are readable on `COUNTERS` alone.
    #[test]
    fn what_times_a_program_or_is_power_needs_the_trace_right() {
        for counter in Counter::ALL {
            assert!(counter.needs().contains(Rights::COUNTERS), "{counter:?}");
            assert_eq!(
                counter.needs().contains(Rights::TRACE),
                !matches!(counter, Counter::Stamp | Counter::Smi | Counter::FirmwareCalls | Counter::FirmwareNanos),
                "{counter:?}"
            );
        }
    }

    #[test]
    fn counters_are_indexed_by_their_place_and_named_once() {
        for (i, counter) in Counter::ALL.iter().enumerate() {
            assert_eq!(*counter as usize, i);
            assert!(Counter::ALL[..i].iter().all(|c| c.name() != counter.name()));
        }
    }
}
