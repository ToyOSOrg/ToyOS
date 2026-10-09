//! The kernel's counters, rendered under `kernel.*`.
//!
//! A root no port answers for, as `dev` is: the reader asks the kernel on a
//! `SysCap` carrying `Rights::COUNTERS`, and `Rights::TRACE` for the counters
//! that time programs and the power envelope, and renders its records here.
//!
//! ```text
//! kernel.cpu.<n>.stale         whether the CPU missed this read's bound
//! kernel.cpu.<n>.hardware_id   the id the CPU read off itself
//! kernel.cpu.<n>.<counter>     each counter the record carries, by its ABI name
//! ```
//!
//! **A counter the record does not carry has no path**, never a zero: the CPU
//! lacks it, or the capability may not read it. A record with no stamp is a CPU
//! whose answer could not be read whole, and renders as its `stale` alone.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;

use toyos_abi::counters::{Counter, Record};

use crate::dev::Repeated;
use crate::wire::Value;

/// The root every counter's path is under.
pub const ROOT: &str = "kernel";

/// Every record as `kernel.*` paths, sorted; a CPU two records name is
/// refused by its path.
pub fn render(records: &[Record]) -> Result<BTreeMap<String, Value>, Repeated> {
    let mut out = BTreeMap::new();
    for record in records {
        let at = format!("{ROOT}.cpu.{}", record.cpu);
        let stale = format!("{at}.stale");
        if out.insert(stale.clone(), record.stale.into()).is_some() {
            return Err(Repeated(stale));
        }
        if record.get(Counter::Stamp).is_none() {
            continue;
        }
        out.insert(format!("{at}.hardware_id"), record.hardware_id.into());
        for counter in Counter::ALL {
            if let Some(value) = record.get(counter) {
                out.insert(format!("{at}.{}", counter.name()), value.into());
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use alloc::vec;
    use alloc::vec::Vec;

    use super::*;
    use crate::check_path;

    fn record(cpu: u32, stale: bool, values: [Option<u64>; Counter::COUNT]) -> Record {
        Record { cpu, hardware_id: cpu * 2, stale, values }
    }

    /// The T14's shape: every counter on every CPU, and the firmware's
    /// calls on the boot processor, which makes them.
    fn whole(cpu: u32) -> Record {
        let (calls, nanos) = if cpu == 0 { (Some(1), Some(15_968)) } else { (None, None) };
        record(cpu, false, [Some(10), Some(4817), Some(100), Some(200), Some(3), Some(0x8000_2a04), Some(0x8000_ff01), Some(6), calls, nanos])
    }

    #[test]
    fn every_counter_a_record_carries_is_a_path_the_grammar_accepts() {
        let got = render(&(0..8).map(whole).collect::<Vec<_>>()).unwrap();
        assert_eq!(got.len(), 8 * Counter::COUNT + 2);
        for path in got.keys() {
            check_path(path).unwrap_or_else(|e| panic!("{path}: {e:?}"));
        }
        assert_eq!(got["kernel.cpu.7.smi"], Value::U64(4817));
        assert_eq!(got["kernel.cpu.7.hardware_id"], Value::U64(14));
        assert_eq!(got["kernel.cpu.7.stale"], Value::Bool(false));
        assert_eq!(got["kernel.cpu.0.firmware_calls"], Value::U64(1));
        assert!(!got.contains_key("kernel.cpu.7.firmware_calls"));
    }

    /// A machine whose CPU counts no SMIs, read on a capability without
    /// `TRACE`: only what the record carries, and no zero in place of the rest.
    #[test]
    fn an_absent_counter_has_no_path() {
        let got = render(&[record(0, false, [Some(10), None, None, None, None, None, None, None, None, None])]).unwrap();
        let paths: Vec<&str> = got.keys().map(String::as_str).collect();
        assert_eq!(paths, vec!["kernel.cpu.0.hardware_id", "kernel.cpu.0.stale", "kernel.cpu.0.stamp"]);
    }

    /// A stale CPU keeps the values it last gave, and says it is stale; one
    /// whose answer was not read says that alone.
    #[test]
    fn a_stale_cpu_says_so_and_an_unread_one_says_only_that() {
        let got = render(&[record(1, true, whole(1).values), record(2, true, [None; Counter::COUNT])]).unwrap();
        assert_eq!(got["kernel.cpu.1.stale"], Value::Bool(true));
        assert_eq!(got["kernel.cpu.1.mperf"], Value::U64(200));
        assert_eq!(got.keys().filter(|p| p.starts_with("kernel.cpu.2.")).count(), 1);
        assert_eq!(got["kernel.cpu.2.stale"], Value::Bool(true));
    }

    #[test]
    fn a_cpu_two_records_name_is_refused_by_its_path() {
        assert_eq!(render(&[whole(3), whole(3)]), Err(Repeated("kernel.cpu.3.stale".into())));
    }
}
