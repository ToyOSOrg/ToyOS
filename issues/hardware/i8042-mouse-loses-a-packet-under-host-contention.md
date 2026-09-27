---
status: expected-red
kind: tooling
opened: 2026-09-27
---

# `i8042_mouse` loses a packet under host contention, three sightings now

Seen on 2026-09-27 in the orchestrator's Fast-tier run on PR #537's head
`06b926b1` — a diff that renames paths only and touches neither the i8042
driver nor this test:

```
FAIL i8042_mouse: 872 pointer events reached userland out of 876 packets
injected, never more than 4 of them (12 bytes) outstanding against a 16-byte
device queue
```

`cargo run -- --known-red i8042_mouse` answered NO. This is the third recorded
sighting of the identical shape and bound (the first two, 2026-08-06/07, are
`issues/build/parallel-tests-red-under-other-suites.md`'s `i8042_mouse` entry,
which this file extends rather than duplicates — read it for the full
history, including the fix that closed the first, different mechanism: a
pacing lead wide enough to make QEMU sum motion it had no room to queue).

## What is known

The test's own design (`tests/toyos.rs`'s `i8042_mouse`) paces its injection so
the host never holds more than `MOUSE_LEAD` (4 packets, 12 of the device's 16
bytes) outstanding, on the stated premise that staying inside what the device
holds should leave no loss to explain away. That bound was already the fix for
the first two sightings' mechanism, and this loss is inside it, so it is not a
recurrence of that one.

`tests/common/qemu.rs`'s own doc comment on `QmpInput::type_burst`, written for
the keyboard path and citing the same `QEMU_PS2_QUEUE` constant this test
budgets against, names a mechanism this test does not guard against: "a guest
whose vCPU the host has not run for a couple hundred milliseconds drains none
of them — at which point the queue starts dropping, silently and one byte at a
time." `MOUSE_LEAD` bounds how much the *host* injects ahead of what it has
seen the guest report; it does not bound how long a starved host leaves that
packet queued before the guest's vCPU runs again to drain it. The keyboard path
closes that gap by pacing against the guest's own echo of each burst
(`shell_type_line`); the mouse path paces against a running count of arrived
events, which is a weaker guarantee against the same starvation this file's
keyboard entries already document. This run's own log shows the host holding
at least three other worktrees' builds at the moment `i8042_mouse` failed
(`[host-builds] … all 4 held by 4 holder(s)`), which is the condition that
mechanism needs.

**Not established as the mechanism** — nothing in this capture names which
byte QEMU dropped or when, and the 2026-08-07 sighting's own A/B (this file's
catalog) left open whether that one was a loss or a miscount. It is offered as
the plausible, code-grounded reading and nothing stronger.

## Exit condition

The mechanism named with evidence (which byte, when, under what host
condition) rather than inferred from a doc comment written for a different
device queue, fixed — most likely by pacing the mouse injection against the
guest's own report the way `shell_type_line` already paces the keyboard's — and
a test that turns red on it deterministically. Then this row, its
`src/redlist.rs` entry, and the corresponding bullet in
`issues/build/parallel-tests-red-under-other-suites.md` are removed.

## Owner

The i8042/input path.
