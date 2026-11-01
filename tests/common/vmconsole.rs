//! The VM's console on a virtio machine: `/system/bin/virtio-console`
//! carrying the log on its port, and the UART taking it back when it goes.

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

/// What the supervisor says of a virtio-serial driver's end, the first
/// program line the UART can carry once the console's has ended. The
/// console's ends on a panic; the clipboard's function's, in an image with no
/// agent, ended with exit 0 at its start.
const DRIVER_ENDED: &str = "supervisor: virtio-console on PCI";
const PANICKED: &str = "ended, code=101";

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
    let ended = |log: &str| {
        log[from..].lines().find(|l| l.contains(DRIVER_ENDED) && l.contains(PANICKED)).map(str::to_string)
    };
    qemu::await_guest(&mut qemu, &mut console, "the console's driver to end", |log| ended(log).is_some())?;
    let line = ended(&console).expect("the wait ended on it");
    let after = Serial::named("console after its port went", console[from..].to_string());
    after.must_say_after(BACK_ON_THE_UART, &line)?;
    eprintln!("  [vmconsole] {}", line.trim());
    Ok(())
}

