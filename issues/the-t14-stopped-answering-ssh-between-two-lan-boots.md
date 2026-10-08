---
status: open
kind: tooling
opened: 2026-09-29
---

# The T14 stopped answering `ssh` between two LAN boots, and the next two arms died before their flash

On the run below neither `lan_lease_report` (boot `lanleasecase`) nor `lan_swap`
(boot `lanswapcase`) reached the stick: the first `ssh` of each timed out, so
neither boot happened and neither judge ran. The suite measured nothing about either test,
and why the bench's `ssh` went away is not established by anything kept.

## Measured

The full T14 run of `main` at `7e151819`
(EXIT=1), in the order the loop ran them:

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

## Exit condition

`lan_swap` is owned by this file until it closes. Three consecutive T14 metal
runs each reach `lanswapcase`'s judge without an `ssh` timeout before its
flash; then this file is deleted. A red on that judge in any of the three is
filed as its own issue file naming the test and its cause before this one
closes.

`lan_swap` and its boot `lanswapcase` are deleted; `issues/a-connect-between-two-accepts-is-reset.md` records the commit that restores them, and this exit waits on that restore.
