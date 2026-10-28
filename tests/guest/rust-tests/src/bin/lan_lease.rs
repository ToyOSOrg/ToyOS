//! The wired link's lease: this job waits on the boot's served log for
//! netstack's lease line, which follows the NIC driver's link and a DHCP
//! exchange with the bench's router, so the boot's log holds both before the
//! list ends it. The line itself is the judge's to read; this says only that
//! it came.

#[path = "../served_log.rs"]
mod served_log;

use std::time::Duration;

/// A liveness ceiling past netstack's own bound on its first lease, never a
/// timing.
const BOUND: Duration = Duration::from_secs(90);

fn main() {
    served_log::Log::open().until("netstack's lease", BOUND, |line| line.contains("netstack: DHCP: lease "));
    println!("lan_lease: netstack leased an address over its link");
}
