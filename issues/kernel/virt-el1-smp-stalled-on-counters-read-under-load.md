---
status: open
kind: defect
opened: 2026-10-04
---

# `virt_el1_smp` stalled waiting for `test_rs_counters_read` to end, under a loaded host

**What was seen.** `cargo test`, the whole guest suite, dev host, on
`wt/toyos-forkpin` at `72621e9cb`, a branch that changes only the build
system's fork checkout and no guest byte:

```
FAIL virt_el1_smp: STALLED: waiting for the job test_rs_counters_read to end — it went quiet
  STALL virt_el1_smp  (33s)
test result: FAILED. 25 passed, 1 failed, 0 invalidated, 26 total
```

The serial the harness printed ends with the guest powering off on its own:
`[kernel 2.408 cpu4] spawn: /system/bin/shutdown`, the supervisor's
`power: the machine stops ... (Shutdown)`, then `[kernel 2.419 cpu0] Shutting
down.` — so the guest did not hang; the job's end never reached the harness
before the machine stopped. The printed serial elides its middle, and
`target/red-run-serial/` kept no capture of this guest, so whether
`counters_read` printed its verdict is not known. In the same run `virt_smp`
(entered at EL2) passed with `counters_read: every cpu answered for itself`.

Host load at the end of the run, from `uptime`: `17.65 28.91 33.61` on 14
cores.

**Exit**: a capture of a red `virt_el1_smp` whole enough to say whether
`counters_read` ended before the shutdown, and the cause it names fixed with
a test that reds without it.
