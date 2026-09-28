//! `SYS_LOG_READ`'s readiness source, probed against a handle close.

use std::path::Path;
use std::time::Duration;

use super::qemu::{BootOptions, QemuInstance};

/// The whole run's ceiling. A liveness guard and never a verdict: the guest has
/// a ceiling of its own and reports what it had when it gave up, so this only
/// catches a guest that stopped answering at all.
const CEILING: Duration = Duration::from_secs(60);

/// A pending poll on the machine's log is not something a handle closing
/// can cancel.
///
/// **The close-cancels-a-foreign-poll defect, gated.** `object::ops::close` handed every source the
/// closing object named to `io_uring::cancel_by_source`, which cancels across every
/// ring in the machine — right for a pipe whose other end has really gone, and
/// wrong for a stream that outlives every handle. Every `SysCap` maps to
/// `Source::Log`, so any process closing any capability posted `-NotFound` into
/// every pending log poll there was. It was latent while nothing parked on one
/// and live from the moment `/system/bin/logd`'s whole loop is read-then-park.
///
/// The verdict is the guest's and it has two halves: closing a second handle to
/// the same capability completes nothing, and a record afterwards still
/// completes the poll — so what the close did not take was a live arming and not
/// an absent one. The immediate half is retried against a record committing in
/// the same microseconds, which is distinguishable because an honest completion
/// leaves the cursor owing records.
pub fn log_poll_outlives_a_close(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    close_probe(test_config, c_bins, rust_bins, &[])
}

fn close_probe(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
    params: &'static [&'static str],
) -> Result<(), String> {
    let mut qemu = QemuInstance::boot_with_options(
        test_config,
        c_bins,
        rust_bins,
        BootOptions { kernel_params: params, ..Default::default() },
    );
    let result = qemu.run_test("log-close", CEILING);
    if let Some(err) = &result.error {
        return Err(format!("{err}\nstdout:\n{}", result.stdout));
    }
    if result.exit_code != Some(0) || !result.stdout.contains("log-close: OK") {
        return Err(format!(
            "the close probe exited {:?}\n{}",
            result.exit_code, result.stdout
        ));
    }
    let survived = result
        .stdout
        .lines()
        .find(|l| l.contains("log-close: survived="))
        .ok_or_else(|| format!("the guest never said what it saw\n{}", result.stdout))?;
    eprintln!("  [log] {}", survived.trim());
    Ok(())
}
