//! The two ways the machine stops, told apart by QEMU rather than by the guest.
//!
//! A guest that resets, one that powers off and one that triple-faults all end
//! a `-no-reboot` QEMU with status 0, so the exit says nothing about which
//! happened. QEMU's `SHUTDOWN` event carries the cause it classified the stop
//! as, and that is what is asserted here: a reboot implemented as a power-off
//! reds on `guest-shutdown`.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use super::qemu::{self, BootOptions, QemuInstance};
use super::serial;

/// The machine returns to firmware when a process holding `POWER` asks it to.
pub fn machine_reboot(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let options = BootOptions { qmp: true, ..Default::default() };
    let mut qemu = QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);

    let boot = serial::Serial::boot(&qemu);
    boot.must_be_clean()?;
    // The half the stop reason cannot see: a kernel that decoded some other
    // register would still reset this machine at 0xcf9 by luck.
    boot.must_say("ACPI: reset register SystemIO 0xcf9 <- 0x0f")?;

    // Opened before the ask: the event is emitted once, and QEMU exits on it.
    let mut stop = qemu::QmpShutdown::open(qemu.qmp_socket());

    writeln!(qemu.stdin_mut(), "run reboot").expect("write to QEMU stdin");
    qemu.flush_stdin();
    let reason = stop.reason(Duration::from_secs(20));
    // Ends when QEMU exits and the reader disconnects, so a machine that came
    // back to firmware costs none of this ceiling.
    let tail = qemu.drain_serial(Duration::from_secs(20));

    let drain = serial::Serial::named("reboot drain", tail.as_str());
    drain.must_be_clean()?;
    drain.must_say("Rebooting.")?;

    match reason.as_deref() {
        Some("guest-reset") => {}
        Some(seen) => {
            return Err(format!(
                "QEMU stopped this guest for {seen:?}, not a guest reset: the machine was not \
                 returned to firmware\n{tail}"
            ))
        }
        None => {
            return Err(format!(
                "QEMU never reported stopping: the guest asked for a reboot and stayed up\n{tail}"
            ))
        }
    }

    eprintln!("  [power] QEMU stopped the guest for guest-reset");
    Ok(())
}
