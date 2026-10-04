---
status: open
kind: finding
opened: 2026-09-18
---

# A CPU that stops passing takes the console with it when `klogd` is queued there

Seen while bounding `nmi_gate`'s hold (PR #470), under a mutation that leaves
the storm's CPU spinning inside its idle-loop hook forever, dev host, TCG,
`-smp 4`:

```
nmi-window-spin: spinning on SYS_GETPID for 10s
nmi-window-spin: 3350000 syscalls in 10020881249 ns (2991 ns each)
===TEST_END test_rs_nmi_window_spin exit=0===
```

Two boots of two, and no kernel record on the console after the spawn lines,
though the dead CPU had logged `syscall-window-nmi: held cpu=…` and another CPU
logged after it. Userland's own lines kept arriving, so the machine and the
UART were alive.

The reading, not yet separated from others: `log::console`'s only drainer once
boot is over is the `klogd` thread, a commit wakes it with `wake_direct`, and a
task queued on a CPU leaves it only when that CPU answers a steal request in a
scheduler pass. A CPU that never passes again answers nothing, so `klogd` woken
onto it stays there and every later record commits to its shard and reaches no
wire. The reviewer's run of the same mutation got the `held cpu=` line out on
one boot of two, which fits: it depends on where `klogd` was queued.

What this costs is the evidence of the failure it coincides with: a wedged CPU
is the case a reader most wants the console for, and
under TCG neither `hardlockup` nor a `WEDGED` verdict named it in either boot.
`nmi_gate::note_syscall` drains inline on its own expiry path for this reason,
which covers that one record and nothing else.

Owed at its next review: whether a wake should prefer a CPU that has passed
recently, or whether this is `hardlockup`'s to name before the console matters.
