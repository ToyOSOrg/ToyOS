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
}

impl Counter {
    pub const COUNT: usize = 10;
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
            | Self::EnergyPerfBias => Rights::COUNTERS.union(Rights::TRACE),
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

    fn record(stale: bool, values: [Option<u64>; Counter::COUNT]) -> Record {
        Record { cpu: 7, hardware_id: 0x0102_0304, stale, values }
    }

    #[test]
    fn every_shape_of_record_reads_back_as_written() {
        for r in [
            record(false, [Some(1), Some(u64::from(u32::MAX)), Some(3), Some(u64::MAX), Some(0), Some(0x8000_2a04), Some(6), Some(0), Some(2), Some(31_936)]),
            record(true, [Some(9), None, None, None, Some(4), None, Some(0x8000_ff01), None, Some(0), None]),
            record(true, [None; Counter::COUNT]),
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
        let mut raw = record(false, [Some(1); Counter::COUNT]).encode();
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
