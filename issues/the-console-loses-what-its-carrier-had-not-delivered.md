---
status: open
kind: defect
opened: 2026-10-24
---

# The console loses what its carrier had not delivered when it went

While `/system/bin/virtio-console` carries the log, the UART takes none of
it (`system/logkeeper/src/carrier.rs`, `kernel/src/log/console.rs`). When the
carrier goes, two runs of lines reach neither the console port nor the UART:

- the bytes logkeeper sent the carrier that its channel had not taken,
  `[delivered, sent)` in `Carrier`'s state;
- the kernel records `klogd` advanced past between the carrier's end and
  logkeeper's word that the UART carries the log again — among them the
  kernel's own record of a driver that crashed, the line that explains the
  event.

The same holds across logkeeper's own death, whose console's last handle
gives the log back: what it had read and not yet written, and the records
`klogd` advanced past before that handle went. `/log` keeps the first run
where logkeeper lived to write it, and the kernel's ring and black box the
second.

**Evidence:** the code; `the_uart_carries_the_log_again_once_the_console_carrier_goes`
shows the restore itself (PR #1062).

**Owner:** unheld; whoever next changes `carrier.rs`.

**Exit:** a carrier that goes mid-stream leaves every line on one of the two
channels: logkeeper, which holds the replay and knows `delivered`, puts the
undelivered tail on the UART as it gives the log back, and the records
`klogd` advanced past since the carrier last delivered reach the UART too.
Shown by a guest test that unplugs the port with lines in flight and finds
each of them once on the merged capture.
