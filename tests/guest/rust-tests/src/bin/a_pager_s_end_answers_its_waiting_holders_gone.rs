//! A pager's end, seen from a reader parked on its object: `pagerprobe`,
//! launched as its row's pager, hands a holder to a helper of its own, waits
//! until its notices say the helper waits on a page it never supplies, and
//! lets its pager go. The helper's read answers `Gone`, and the probe says so
//! by its exit. The probe also asserts the kernel refuses a pager a holder's
//! read of its own object.

use std::os::toyos::process::CommandExt;
use std::process::Command;

fn main() {
    let status = Command::new("/system/bin/pagerprobe")
        .independent()
        .status()
        .expect("the pager probe launches: testcases' test-runner starts it");
    assert!(status.success(), "the pager probe ended {status:?}");
    println!("a_pager_s_end_answers_its_waiting_holders_gone: ok");
}
