//! Idle-suspend certification: on a boot where no audio client ever
//! connects, soundd's CPU cost is exactly zero. Two sysinfo samples ~1s apart
//! must show no cpu_ns movement on any soundd thread — a suspended soundd
//! holds no timer and takes no wakes, so any nonzero delta is the mix or
//! control loop running without a reason. This is the one idle-suspend claim gate A
//! structurally cannot see: its counters are streaming-scoped, and its boots
//! always connect a client. No wav analysis — there is no signal, and the
//! capture freezes while the voice is stopped anyway.
//!
//! Another process's threads is the process roster, which costs
//! `Rights::ROSTER` on a `SysCap` — `tests/testcases` names `roster` on the
//! test-runner row and every binary it spawns holds the duplicate. A capability
//! this binary does not hold is a config fact and says so, rather than reading
//! back zero soundd threads and calling that a suspended daemon.

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::syscap::SysCap;
use toyos::system;

const HEADER: usize = system::SYSINFO_HEADER_SIZE;
const ENTRY: usize = system::SYSINFO_ENTRY_SIZE;

/// Live cpu_ns per soundd thread. The mix thread reports under the process
/// name ("soundd"), the control thread under its own ("soundd-ctrl");
/// matching the prefix covers both.
fn soundd_threads(cap: &SysCap) -> Vec<(String, u64)> {
    let mut buf = vec![0u8; HEADER + ENTRY * 128];
    let n = cap.roster(&mut buf);
    assert!(n >= HEADER, "sysinfo failed");

    let mut threads = Vec::new();
    let mut pos = HEADER;
    while pos + ENTRY <= n {
        let name_bytes = &buf[pos + 32..pos + 60];
        let len = name_bytes.iter().position(|&b| b == 0).unwrap_or(28);
        if name_bytes[..len].starts_with(b"soundd") {
            let name = String::from_utf8_lossy(&name_bytes[..len]).into_owned();
            let cpu_ns = u64::from_le_bytes(buf[pos + 24..pos + 32].try_into().unwrap());
            threads.push((name, cpu_ns));
        }
        pos += ENTRY;
    }
    threads
}

fn main() {
    let cap: SysCap = Endowments::get()
        .take(SYSCAP_LABEL)
        .expect("test-runner endows every binary it spawns a system capability");
    // soundd's control thread is spawned after the mix thread, so a roster
    // read can land between the two; the measurement starts once both exist.
    let mut before = soundd_threads(&cap);
    let mut waited_ms = 0u32;
    while before.len() < 2 && waited_ms < 5_000 {
        std::thread::sleep(std::time::Duration::from_millis(20));
        waited_ms += 20;
        before = soundd_threads(&cap);
    }
    assert!(
        before.len() >= 2,
        "expected soundd's mix and control threads in sysinfo, found {} after {waited_ms}ms",
        before.len()
    );
    std::thread::sleep(std::time::Duration::from_millis(1000));
    let after = soundd_threads(&cap);
    assert!(
        after.len() >= 2,
        "soundd lost a thread mid-sample: found {} of the 2 the first sample saw \
         (before {before:?}, after {after:?})",
        after.len()
    );
    let total_before: u64 = before.iter().map(|(_, ns)| ns).sum();
    let total_after: u64 = after.iter().map(|(_, ns)| ns).sum();
    assert_eq!(
        total_after, total_before,
        "soundd consumed {}ns of CPU across ~1s with no client — it is not suspended \
         (per thread, before {before:?}, after {after:?})",
        total_after.saturating_sub(total_before)
    );
    println!("soundd idle cpu delta: 0ns over ~1s");
}
