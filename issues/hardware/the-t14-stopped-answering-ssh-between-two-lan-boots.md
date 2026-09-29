---
status: expected-red
kind: tooling
opened: 2026-09-29
---

# The T14 stopped answering `ssh` between two LAN boots, and the next two arms died before their flash

`lan_lease_report` (boot `lanleasecase`) and `lan_swap` (boot `lanswapcase`)
are the two arms the metal suite flashes after `lanicscase`. On the run below
neither reached the stick: the first `ssh` of each timed out, so neither boot
happened and neither judge ran. The suite measured nothing about either test,
and why the bench's `ssh` went away is not established by anything kept.

## Measured

The full T14 run of `main` at `7e151819`
(`/Users/jan/.claude/jobs/2280e09e/tmp/scratchpad/orch/main-metal-full.log`,
EXIT=1), in the order the loop ran them:

```
[metal] lanicscase: ...
the machine answered ssh again after 20 s
PASS: the machine booted ToyOS in 1165 ms
[metal] lanleasecase: ...
toyos-metal: reading the disk on the machine exited exit status: 255: ssh: connect to host t14 port 22: Operation timed out
[metal] lanswapcase: ...
toyos-metal: `sudo -n` on the machine did not answer: sudo -n on the machine exited exit status: 255: ssh: connect to host t14 port 22: Operation timed out. Install the rule first
toyos-metal: the swap did not put the new binary in service:
  no boot opened the record stream within 420 s
[metal] lantalkcase: ... (flashed, booted and read back)
  FAIL lan_lease_report: toyos-metal exited exit status: 2
  FAIL lan_swap: toyos-metal exited exit status: 2
```

Host file times under `/Users/jan/Dev/jan/toyos-metalmain/target/metal/`:
`lanicscase/boot.txt` 12:47:18, `lanswapcase/swap-stream.log` 12:50:47,
`lantalkcase/stream.log` 12:58:43 — the machine answered `ssh` for the
`lanicscase` readback and again for the `lantalkcase` flash, and not between.
`lanicscase`'s 20 s back is the shortest of the run; every other boot's is
44 s or more, excluding `lanswapcase` and `lanleasecase`, which have none.

The orchestrator reports both names red on an earlier T14 run of the same
`main` as well; that run's log was lost, and its cause is not known.

## Exit condition

A T14 run in which `lanleasecase` and `lanswapcase` both reach their judges.
Green: their rows in `src/redlist.rs` and this file are deleted. Red on a
judge: each red is filed with its own cause and its row moved to it.
