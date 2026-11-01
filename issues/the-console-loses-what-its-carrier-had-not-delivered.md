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

**A carrier that is stuck but alive** — a driver that stops reading its pipe,
or a device that never completes a transmit — no longer leaves the UART
silent: once the channel has taken nothing it is owed for `STALL` (5 s),
logkeeper tells the kernel the UART carries the log again and keeps feeding
the carrier, and quiets it once the channel takes more
(`Carriage::tick`). What the channel had not taken when the stall was
declared is on neither channel, as above, and while the UART speaks beside a
carrier that recovers, lines can reach both.

**Residual: logkeeper itself stuck but alive.** A logkeeper that deadlocks,
or whose stall thread cannot run, still holds its console, so the kernel
keeps the UART quiet and `klogd` advances past every record until the stop
or a fatal path. Nothing in the kernel watches whether the holder still
reads the log, and the kernel keeps no per-reader cursor to watch. The
proposed exit for this part is a lease: a carry expires unless its holder
renews it within a bound, and logkeeper's main loop renews it as it reads the
ring, so a logkeeper that stops reading gives the UART the log back by the
lease's end.

**Evidence:** the code; `the_uart_carries_the_log_again_once_the_console_carrier_goes`
shows the restore itself (PR #1062).

**Owner:** unheld; whoever next changes `carrier.rs`.

**Exit:** logkeeper's own stall covered by the lease above, and a carrier
that goes mid-stream leaves every line on one of the two channels:
logkeeper, which holds the replay and knows `delivered`, puts the
undelivered tail on the UART as it gives the log back, and the records
`klogd` advanced past since the carrier last delivered reach the UART too.
Shown by a guest test that unplugs the port with lines in flight and finds
each of them once on the merged capture.
