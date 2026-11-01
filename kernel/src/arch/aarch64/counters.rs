//! No model-specific counter is read on this architecture: the activity
//! monitors' cycle counters (`FEAT_AMUv1`) are unread, and no performance
//! request is declared.

use crate::counters::Hardware;

pub fn bring_up() {}

pub fn read() -> Hardware {
    Hardware {
        smi: None,
        aperf: None,
        mperf: None,
        envelope: None,
        firmware: None,
        core_c6: None,
        core_c7: None,
        package: None,
        package_residency: None,
        core_energy: None,
    }
}
