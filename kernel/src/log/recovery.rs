//! The black box's recovery section: the USB stack's records from the boot's
//! first transport break on, read off the ring for whichever seal is ending
//! this boot.
//!
//! Which records, how many and where they go are `toyos_blackbox::Recovery`'s
//! and `Kept`'s; this walks the ring for them. **Lock-free and allocation-free,
//! because two of its three callers are the panic path and an interrupt entry
//! on a wedged machine**: [`read::snapshot_committed`] is the one reader either
//! may use.

use toyos_abi::log::LogRecord;
use toyos_blackbox::{Kept, Recovery, Report};

use super::read::{self, RecordSink};
use super::shard::Origin;

/// Write the section at `report`'s end.
pub fn seal_into(report: &mut Report<'_>) {
    struct Measure(Recovery);
    impl RecordSink for Measure {
        fn put(&mut self, record: &LogRecord, _origin: Origin) -> bool {
            self.0.saw(record.at_ns, record.message(), record);
            true
        }
    }
    struct Place<'k, 'r, 'a>(&'k mut Kept<'r, 'a>);
    impl RecordSink for Place<'_, '_, '_> {
        fn put(&mut self, record: &LogRecord, _origin: Origin) -> bool {
            self.0.put(record.message(), record)
        }
    }

    // One upper bound for both walks: a record committed between them is
    // stamped past it, so the second walk places what the first measured.
    let to = read::newest_committed_at_ns();
    let mut measure = Measure(Recovery::new());
    read::snapshot_committed(0, to, &mut measure);
    let mut kept = report.recovery(measure.0);
    if let Some(from) = kept.from() {
        read::snapshot_committed(from, to, &mut Place(&mut kept));
    }
    kept.close();
}
