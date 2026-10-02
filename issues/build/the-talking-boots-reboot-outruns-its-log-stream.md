---
status: open
kind: tooling
opened: 2026-09-29
---

# The talking boot's `reboot` outruns its log stream, so `lan_talk` reds when the backlog has not crossed

`metaltalk::converse` (`src/metaltalk.rs`) waited for the stream to connect,
pinged, ran one command and fired `reboot`; nothing waited on the stream
carrying the lines `metaltalk::judge` then requires, this boot's
`Boot: complete` among them. `logd` serves the boot from its first line, so
the verdict was a race between the backlog crossing and the `reboot` the host
itself sends. `metaltalk::hand_back` now fires `reboot` once the stream has
carried that record or its bound has passed.

## Measured

The full T14 run of `main` at `7e151819`
(EXIT=1), boot `lantalkcase`:

```
toyos-metal: the boot did not say over its own cable what a talking boot owes:
  217 line(s) arrived over the cable and none is this boot's `Boot: complete`
  FAIL lantalkcase: toyos-metal exited exit status: 1
  FAIL lan_talk: toyos-metal exited exit status: 1
```

Its readback:
`talk.txt` says the ping, the command (`status 0`, 495 ms) and the `reboot`
all succeeded, with `talk_stream_end open`. `stream.log` is `kernel.log`'s
first 217 lines, ending at `0.493 ... xHCI: configuration set`;
`Boot: complete (1167ms)` is `kernel.log:289`. `kernel.log` has
`logd: serving this boot's log to 192.168.1.47:54752` at 20.996 s and init's
`power: the machine stops` at 21.591 s: the stream had 595 ms.

The T14 run of PR #609 at `7d2e15c0` gave the same finding at the same 217th
line, with 262 ms between the admit and the stop. The stall is at a fixed
byte count, not a fixed time. netd's I219 drops everything past 15 frames in
flight (`issues/hardware/netds-i219-drops-a-transmit-burst-past-its-ring.md`),
and the rest of the stream waits for a retransmit timeout that `reboot` beat.

## Exit condition

Two consecutive T14 runs pass `lan_talk`; then this file is deleted.
