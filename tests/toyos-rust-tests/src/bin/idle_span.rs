//! **The machine idles for a span nothing can mistake for work**, and this
//! process's exit is the boot's first. It asserts nothing: `mask_windows`
//! reads what a `mask-windows` kernel reports of the CPU that slept through
//! it, whose halt is no preemption-off window.

use std::thread::sleep;
use std::time::Duration;

/// Five times what the record's ceiling allows the window the kernel holds at
/// this exit (`toyos_sched::windows::HELD_NS`, doubled), so a halt counted as
/// a window is past it.
const SPAN: Duration = Duration::from_millis(500);

fn main() {
    sleep(SPAN);
}
