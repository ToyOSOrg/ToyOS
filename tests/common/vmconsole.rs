//! The VM's console on a virtio machine: `/system/bin/virtio-console`
//! carrying the log on its port, and the UART taking it back when it goes.

use std::io::Write;
use std::path::Path;

use super::qemu::{self, BootOptions, QemuInstance, QmpMonitor};
use super::serial::Serial;

/// What `logkeeper` says once the console's carrier has taken the log's first
/// bytes and the kernel has been told the UART is quiet
/// (`system/logkeeper/src/carrier.rs`).
const QUIETED: &str = "logkeeper: the console carrier's channel takes the log, so the UART is quiet";

/// What the kernel puts on the UART as the log comes back to it
/// (`kernel/src/log/console.rs`).
const BACK_ON_THE_UART: &str = "console: the UART carries the log again";

/// What the supervisor says of the console's driver ending, the first
/// program line the UART can carry once it has.
const DRIVER_ENDED: &str = "supervisor: virtio-console on PCI";

/// **The UART carries the log again once the console's carrier goes.** The
/// console's port is unplugged under the running guest, the one way to end
/// its driver without the authority to: the driver ends on the device's
/// `DEVICE_REMOVE`, `logkeeper` sees its carrier gone, the kernel says so on
/// the UART, and the supervisor's account of the driver's end reaches the
/// harness there. Its parts are host-tested (`logkeeper`'s `Carriage`,
/// `kernel::carrier`); what only a guest has is the kernel, `logkeeper` and
/// the driver's process doing it. q35 only: no other machine of this suite
/// carries a console port.
pub fn the_uart_carries_the_log_again_once_the_console_carrier_goes(test_config: &Path) -> Result<(), String> {
    let mut qemu = QemuInstance::boot_with_options(test_config, &[], &[], BootOptions { qmp: true, ..Default::default() });
    let boot = Serial::boot(&qemu);
    boot.must_be_clean()?;
    let mut console = boot.text().to_string();
    qemu::await_marker(&mut qemu, &mut console, QUIETED, "logkeeper to quiet the UART for its carrier")?;
    let from = console.len();
    QmpMonitor::open(qemu.qmp_socket()).human(&format!("device_del {}", toyos_build::vmconsole::PORT_DEVICE));
    qemu::await_marker_new(&mut qemu, &mut console, DRIVER_ENDED, from, "the console's driver to end")?;
    let after = Serial::named("console after its port went", console[from..].to_string());
    after.must_say_after(BACK_ON_THE_UART, DRIVER_ENDED)?;
    eprintln!("  [vmconsole] {}", after.must_say(DRIVER_ENDED)?.trim());
    Ok(())
}

/// What the fatal path says once it has armed the reboot that ends it
/// (`kernel/src/panic_reboot.rs`): a line only the UART carries.
const REBOOT_ARMED: &str = "panic: rebooting in";

/// **A kernel's fatal report reaches the harness on the UART while the
/// console port's carrier carries the log**, three CPUs making kernel
/// records as fast as they can — the records `klogd` advances past unwritten
/// while it does. `test_rs_panic_halts_first` goes fatal by `SYS_DEBUG` once
/// its siblings run: its line and the fatal path's after it are the UART's
/// alone, the carrier being one of the processes the fatal path stopped.
/// That a fatal path's first record is never advanced past is the ordering
/// `kernel/src/log/console.rs` states, not this test's: the halt makes the
/// same claim microseconds later. q35 only: no other machine of this suite
/// carries a console port.
pub fn a_fatal_report_reaches_the_uart_while_a_carrier_carries_the_log(test_config: &Path) -> Result<(), String> {
    let fatal = qemu::build_toyos_bin(
        qemu::SUITE_ARCH,
        &super::compile::repo_root().join("tests/guest/rust-tests"),
        "panic_halts_first",
    );
    let options = BootOptions {
        smp: crate::STOP_CPUS,
        kernel_features: crate::ACTUATOR_KERNEL,
        extra_root_files: vec![("bin/test_rs_panic_halts_first".to_string(), fatal)],
        ..Default::default()
    };
    let mut qemu = QemuInstance::boot_with_options(test_config, &[], &[], options);
    let mut console = Serial::boot(&qemu).text().to_string();
    qemu::await_marker(&mut qemu, &mut console, QUIETED, "logkeeper to quiet the UART for its carrier")?;
    let from = console.len();
    writeln!(qemu.stdin_mut(), "run test_rs_panic_halts_first").map_err(|e| e.to_string())?;
    qemu.flush_stdin();
    qemu::await_marker_new(&mut qemu, &mut console, REBOOT_ARMED, from, "the fatal path to arm its reboot")?;
    let after = Serial::named("console after the fatal halt", console[from..].to_string());
    let nonce = after.must_say_after(crate::FATAL_HALT_NONCE, REBOOT_ARMED)?;
    eprintln!("  [vmconsole] {}", nonce.trim());
    Ok(())
}
