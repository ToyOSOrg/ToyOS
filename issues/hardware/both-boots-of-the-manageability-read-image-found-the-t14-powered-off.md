---
status: open
kind: defect
opened: 2026-09-21
---

# Both boots of the manageability-read image found the T14 powered off

A read-only netd job — one read of the I219's `EXTCNF_CTRL` (§10.2.2.15,
`0x00F00`), no write, no PHY register touched, then `exit` with the reading
encoded in the code — was flashed to the T14 twice, as metal runs 58 and 62.
Both times the owner came back to a machine that was **off**: not frozen, not
wedged at a panic, powered down. Neither boot left a black box.

The persisted kernel log is the same on both runs: it ends at the first `logd`
batch, 192 lines, at around 0.6 s. So the stick carries nothing from the moment
the machine went away, and nothing recorded says what took the power.

The cause is unknown. The arm has been removed from the tree rather than left
armed: the reading it was after is asked through the PHY probe's exit code
instead.

This closes when a boot records what happened — a black box, a log that reaches
past 0.6 s, or a firmware event log naming the shutdown — or when nothing needs
that experiment again.
