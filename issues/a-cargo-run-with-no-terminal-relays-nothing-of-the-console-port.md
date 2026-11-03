---
status: open
kind: defect
opened: 2026-11-03
---

# A `cargo run` with no terminal relays nothing of the console port

`cargo run -- --arch aarch64`, launched with stdin on `/dev/null` and
stdout and stderr on a file, at #934's `ded3c605c`, QEMU 11.1.1 under HVF:

- The boot ran. Every PCI claim was handed over: the NVMe, virtio-net and
  both virtio-serial functions. The UART's file
  (`/tmp/toyos-uart-early.log`) reached
  `console: the log is carried elsewhere from here` at 3.437 s, once
  logkeeper's carrier had the log.
- The launch's own output then held one kernel line, at 3.451 s, and
  nothing more in five minutes. That is everything the console port relays,
  so the port relayed nothing of the replay the carrier owes from the
  boot's first line.
- The same launch under `script -q` (a pseudo-terminal) showed the whole
  log on its terminal: the disk driver serving its partition and
  `vdagent: the host announces capabilities`.

The port's chardev is QEMU's `stdio` (`build/vmconsole.rs`, `args`), which
reads the launch's stdin. One unconfirmed explanation is that a stdin at
end of file closes that chardev, and the guest's port with it. Then the
carrier would have nowhere to write, but the UART would have to take the
log back when the carrier goes, and no such line was read. Neither
explanation is established, and x86-64 was not tried the same way.

Logs: ToyOSOrg/evidence `pr-934`
`6e68eedc826f28ff94bf6177631f1773d387d997`,
`combined-r5/arm-cargo-run.log` and `combined-r5/arm-uart-1.log`; the
pseudo-terminal run beside them as `arm-cargo-run-tty.log`.

**Owner**: whoever holds the console, `build/vmconsole.rs`.

**Exit**: a launch with no terminal either shows the log the port carries,
or is refused at the launch by name. The cause is read from QEMU's
`chardev/char-stdio.c` or a run, and stated.
