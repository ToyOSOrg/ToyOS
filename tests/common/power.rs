//! The two ways the machine stops, told apart by QEMU rather than by the guest:
//! a reset, a power-off and a triple fault all end a `-no-reboot` QEMU with
//! status 0, so what is asserted is the cause its `SHUTDOWN` event names, and a
//! reboot implemented as a power-off reds on `guest-shutdown`. Nothing here
//! judges the boot after the reset: `-no-reboot` exits instead of taking it.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use super::qemu::{self, BootOptions, QemuInstance};
use super::serial;

const WAIT: Duration = Duration::from_secs(20);

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
    // A decode this kernel got wrong, never one it bypassed: a kernel writing
    // 0xcf9 without reading the FADT satisfies this and the stop reason both.
    boot.must_say("ACPI: reset register SystemIO 0xcf9 <- 0x0f")?;

    let mut stop = qemu::QmpShutdown::open(qemu.qmp_socket(), qemu.budget(WAIT));

    writeln!(qemu.stdin_mut(), "run reboot").expect("write to QEMU stdin");
    qemu.flush_stdin();
    let reason = stop.reason();
    // Ends when QEMU exits and the reader disconnects, so a guest that came back to firmware pays none of this.
    let tail = qemu.drain_serial(WAIT);

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

/// The chipset resets a machine whose kernel stops feeding its watchdog.
///
/// The reset is the chipset's, not the kernel's: `watchdog_fed` is the same
/// guest with the feed left on, and it is what says the bound is real rather
/// than a timer nothing was ever going to reload.
pub fn watchdog_resets(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let options = starved();
    // QEMU's chipset holds `NO_REBOOT` set unless the argv clears it, and an
    // expiry under that strap does nothing at all — which is a green run for
    // the wrong reason if the option ever goes silently inert.
    let argv = qemu::profile_argv(&options);
    if !argv.windows(2).any(|w| w[0] == "-global" && w[1] == "ICH9-LPC.noreboot=false") {
        return Err(format!("the guest may not be reset by its own chipset: {argv:?}"));
    }

    let mut qemu = QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);
    let boot = serial::Serial::boot(&qemu);
    boot.must_be_clean()?;
    boot.must_say("watchdog: 8086:2918 TCO at 0x660 TCO_TMR=2")?;

    let mut stop = qemu::QmpShutdown::open(qemu.qmp_socket(), qemu.budget(WAIT));
    let reason = stop.reason();
    let tail = qemu.drain_serial(WAIT);

    match reason.as_deref() {
        Some("guest-reset") => {}
        Some(seen) => {
            return Err(format!("QEMU stopped this guest for {seen:?}, not a reset\n{tail}"))
        }
        None => {
            return Err(format!(
                "the chipset never reset a guest that stopped feeding it\n{}{tail}",
                boot.text()
            ))
        }
    }

    eprintln!("  [power] the chipset reset a guest that stopped feeding it");
    Ok(())
}

/// The control: the same guest, feeding, runs past the bound and is still there.
pub fn watchdog_fed(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let options = BootOptions { kernel_params: &["tco-arm", "tco-fast"], ..starved() };
    let mut qemu = QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);
    serial::Serial::boot(&qemu).must_be_clean()?;

    // Long enough for the bound the arm line reports to have passed several
    // times over, which is what makes the absence below mean anything.
    let mut stop = qemu::QmpShutdown::open(qemu.qmp_socket(), FED_FOR);
    if let Some(seen) = stop.reason() {
        let tail = qemu.drain_serial(WAIT);
        return Err(format!(
            "QEMU stopped a guest that was feeding its watchdog, for {seen:?}\n{tail}"
        ));
    }

    // Not merely un-stopped: still answering, so the absence is a live machine
    // rather than one that wandered off before the chipset could act.
    let result = qemu.run_test("pwd", Duration::from_secs(30));
    if result.exit_code != Some(0) {
        return Err(format!("the guest stopped answering after {FED_FOR:?}: {result:?}"));
    }

    eprintln!("  [power] a fed guest ran {FED_FOR:?}, several bounds, and still answers");
    Ok(())
}

/// The guest both watchdog tests boot: the target laptop's shape, its chipset
/// allowed to reset it, and the TCO armed at seconds with the feed cut.
fn starved() -> BootOptions {
    BootOptions {
        profile: qemu::Profile::Metal,
        qmp: true,
        watchdog_resets: true,
        kernel_params: &["tco-arm", "tco-fast", "tco-starve"],
        ..Default::default()
    }
}

/// Several times the bound `tco-fast` arms, so a machine that was going to be
/// reset has been.
const FED_FOR: Duration = Duration::from_secs(12);
