//! No model-specific counter is read on this architecture: the activity
//! monitors' cycle counters (`FEAT_AMUv1`) are unread.

use crate::counters::Hardware;

pub fn bring_up() {}

pub fn read() -> Hardware {
    Hardware { smi: None, aperf: None, mperf: None }
}
