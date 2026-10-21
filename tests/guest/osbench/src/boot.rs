//! Boot to userland, as each system can say it unprivileged. **The two boot
//! different things**: a ToyOS benchcase boot starts four services and the
//! runner, a Linux distribution its whole session; the numbers say where each
//! boot's time went, not which is better.
//!
//! ToyOS: the CPU's counter runs from reset, and the clock page says where
//! the kernel's zero is on it, so `boot.kernel_at_ms` is firmware and loader and
//! `boot.this_job_at_ms` the kernel and userland up to this job, which the job
//! list runs first. The kernel's own `boot:` lines in the log split the
//! kernel's part further. Linux: `systemd-analyze time`, as it prints it,
//! where it answers; and how long ago the kernel started.

use crate::report::condition;

pub fn suite() {
    #[cfg(target_os = "toyos")]
    {
        let page = toyos_abi::clock::page();
        let since_kernel = toyos_abi::clock::nanos_since_boot();
        condition("boot.kernel_at_ms", page.stamp_at_boot / 1_000_000);
        condition("boot.this_job_at_ms", since_kernel / 1_000_000);
        condition("boot.this_job_since_counter_zero_ms", (page.stamp_at_boot + since_kernel) / 1_000_000);
    }
    #[cfg(target_os = "linux")]
    {
        let uptime = std::fs::read_to_string("/proc/uptime").unwrap_or_default();
        condition("boot.uptime_s", uptime.split_whitespace().next().unwrap_or("unknown"));
        match std::process::Command::new("systemd-analyze").arg("time").output() {
            Ok(out) if out.status.success() => {
                let text = String::from_utf8_lossy(&out.stdout);
                for (i, line) in text.lines().filter(|l| !l.trim().is_empty()).enumerate() {
                    condition(&format!("boot.systemd_analyze.{i}"), line.trim());
                }
            }
            Ok(out) => crate::report::refuse(
                "boot.systemd_analyze",
                format!("ended {}: {}", out.status, String::from_utf8_lossy(&out.stderr).trim()),
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                crate::report::unsupported("boot.systemd_analyze", "this system has no systemd-analyze")
            }
            Err(e) => crate::report::refuse("boot.systemd_analyze", format!("would not run: {e}")),
        }
    }
}
