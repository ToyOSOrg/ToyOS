---
status: open
kind: tooling
opened: 2026-11-05
---

# A red run keeps no serial log for a boot whose UART is QEMU's stdio

A suite that is not green copies every `uart-*.log` under its scratch to
`target/red-run-serial/` (`tests/common/lane.rs`'s `keep_serial`), which
`guest-rows.yml` uploads as `serial`. A boot writes `uart-<seq>.log` only
where its UART is on the socket the harness listens on, a profile with the
console's port (`toyos_build::vmconsole::UartSocket`). Where the 16550 is
QEMU's stdio, the console exists only in the harness's memory, so a red boot
of such a profile leaves no file.

A console-port boot's console is not kept either; only its UART is. Its
console is the virtio-serial port on QEMU's stdio, which the harness also
holds only in memory (`tests/common/qemu.rs`'s `read_consoles`), and its
`uart-<seq>.log` holds what the loader said and the lines the port cannot
carry.

Guest-rows 42010705757 reddened `acpi_press_is_one_sci`
(`Profile::HeadlessNoConsolePort`) with a STALL, and its `serial` artifact
held one file: lane-2's `uart-4.log`, a passing console-port boot of
`acpi_holder_is_refused_a_held_functions_configuration`. The stalled boot's
console survived only as the failure message's tail.

**Exit**: every boot of a red run leaves its whole console under
`target/red-run-serial/`, whatever carries it.
