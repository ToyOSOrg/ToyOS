---
status: open
kind: defect
severity: [data-loss]
opened: 2026-10-24
---

# The console loses what its carrier had not delivered when it went

Each of the console's channels — the VM's console port and the console UART —
is a userland driver `logkeeper` feeds (`system/logkeeper/src/carrier.rs`).
While the port carries the log, the UART is fed none of it. Two runs of
lines can reach neither channel:

- the bytes logkeeper sent a carrier that its channel had not taken when the
  carrier went, `[delivered, sent)` in its `Carriage`; the UART's carrier is
  fed from the moment the port's goes, not from what the port had not taken;
- at the machine's stop, the program lines in the UART's pipe that the UART
  had not taken when userland stopped. The kernel's stop and every fatal path
  put on the UART every kernel record past the read positions of the rounds
  a channel delivered whole (`log::console`), so no kernel record is lost
  this way.

The same holds across logkeeper's own death: its carriers' pipes end, and
the UART carries nothing until the kernel's stop or a fatal path. `/log`
keeps what logkeeper lived to write, and the kernel's ring and black box the
kernel's records.

**A carrier that is stuck but alive** — a driver that stops reading its pipe,
or a device that never completes a transmit — no longer leaves the UART
silent: once the port's channel has taken nothing it is owed for `STALL`
(5 s), the UART is fed the log again beside it, and quiet once the channel
takes more (`Carriage::tick`). What the channel had not taken when the stall
was declared is on neither channel, as above, and while the UART speaks
beside a carrier that recovers, lines can reach both.

**A refused stop's lines the UART took reach it twice.** While the machine
stops, the lines said are held back from `/log` and handed the UART's carrier
as they are said (`Hub::append_stopping`); a refused stop writes them to the
file and the replay (`Hub::resume`), and the UART's carrier is fed them again
there, so those it had already taken repeat. `/log` holds them once.

**Residual: logkeeper itself stuck but alive.** A logkeeper that deadlocks
feeds neither channel, and nothing watches whether it still reads the ring.

**Evidence:** the code.

**Owner:** unheld; whoever next changes `carrier.rs`.

**Exit:** a refused stop's lines reach the UART once: `Hub::resume` moves the
UART's carrier past the bytes it already took as the stop's. And a carrier
that goes mid-stream leaves every line on one of the two channels:
logkeeper, which holds the replay and knows `delivered`, feeds the UART from
the port's undelivered tail as it gives the log back; and the stop
waits, bounded, for the UART to take what it was fed. Shown by a guest test
that unplugs the port with lines in flight and finds each of them once on the
merged capture.
