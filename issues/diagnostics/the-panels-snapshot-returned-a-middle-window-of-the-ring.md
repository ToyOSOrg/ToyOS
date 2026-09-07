---
status: open
kind: defect
opened: 2026-09-07
---

# The panel's snapshot of the log ring returned a middle window, and neither of its two caps was reached

T14 run 21, `deadlinewedge` on `8cc5dbee`. The boot deadline expired at 120153 ms
and sealed `live_tail()` into the black box; the next loader pass printed the
record into `loader.log`. What it printed is **not the tail of the ring**:

| | span | records |
|---|---|---:|
| what `logd` wrote to the stick | 0.000 → 2.208 s | 295 lines |
| what the seal carried | **0.148 → 1.210 s** | **190 lines** |
| dropped at the old end | before 0.148 s | 69 |
| dropped at the **new** end | after 1.210 s | 10 |

The ten lost at the new end are the ones the record exists for: `spawn:
/system/bin/logd`, `spawn: /system/bin/test-runner`, the `i8042` verdict, the
`usb-storage` flush line, `spawn: /system/bin/reboot pid=6` — and
`wedge: staged`, which is the witness the harness asserts on.

## Neither cap was reached, which is what makes this a defect and not a bound

- **`Rendered`'s buffer**: `SNAPSHOT_CAP` is 32 KiB
  (`kernel/src/drivers/panic_console/mod.rs:41`). The sealed text is
  **13,948 bytes** as printed, ~13.6 KiB before the loader's `| ` prefixes. The
  whole ring would have been about 19 KiB. `Backfill::put` never returned
  `false`.
- **The black box**: `TEXT_BYTES` is 16,344 and the kernel's own line says so
  (`black box: 0x8000000 is this boot's, 16344 bytes for the next boot's
  loader`). `Report::tail` takes the `records.len() <= room` branch below that
  and writes everything it is handed, so it did not cut either end — and if it
  *had* cut, it cuts the head and keeps the tail, which is the opposite of what
  is missing.

So `live_tail()` — `Rendered::render(0, u64::MAX)` over
`log::read::snapshot_committed` — returned 190 of 295 committed records, missing
both ends, with 18 KiB of buffer free. Read against the code, every piece of
that path looks right: `snapshot_committed` descends newest-first by `at_ns`
across the shards, `Backfill` writes back-to-front so the newest survive a full
buffer, `Descent::advance` walks down to `oldest_readable()` and `SHARD_RECORDS`
is 512 against a shard that wrote at most ~250. Static reading does not explain
it.

## It does not reproduce on QEMU, and the difference is the machine

`boot_deadline_ends_a_wedge` asserts `must_say_after(PREVIOUS_PANIC,
WEDGE_STAGED)` — the seal carrying the newest record — and it is green: 18.3 s,
repeatedly. That guest boots in 0.43 s with two CPUs and about 60 records. The
T14 boots in 1.2 s with **eight** CPUs and 295 records across eight shards. The
shape of what is missing — a contiguous window in *timestamp* order, not a
per-shard one — is the merge's, so the CPU count and the record count are where
to look.

## Why it matters more than a truncated report

This is the read path behind **three** instruments, not one: the panic panel's
report, Ctrl+Alt+D's machine-wide dump, and now the boot deadline's sealed
record. On a machine with no serial port the third is the only channel a wedged
boot has, and
`issues/hardware/a-t14-boot-wedges-after-a-jobs-exit-and-nothing-said-why.md`
is waiting on exactly the records this dropped. A seal that loses its newest ten
records is a seal that cannot name a wedge.

**Not reproduced yet, and here is why**: a guest build on this branch is refused
while another worktree holds the shared sysroot, so the eight-CPU, long-boot
reproduction this needs has not been run. It is the next step and it is cheap —
a `jobcase` boot at `smp=8` with enough records to exceed 190, asserting that
the sealed text's last line is the kernel's last record.

**Exit condition**: that reproduction, red before the fix and green after, and
the seal's last line being the ring's newest record on the T14.
