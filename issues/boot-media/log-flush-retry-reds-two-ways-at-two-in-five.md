---
status: open
kind: tooling
opened: 2026-09-01
---

# `log_flush_retry` reds two out of five runs, on two different assertions, and is not on the redlist

Measured on the dev host in one session while gating a branch whose whole diff
is doc comments and one tracker file. Five runs of `cargo test --test
toyos-build -- --nightly log_flush_retry` on the branch head
(`08ba695e`, `main` `750b1a72` merged in): **runs 1, 3, 4 green; runs 2 and 5
red.** Five runs of the same command on `750b1a72` itself, same session, same
host, detached: **run 2 red, four green.** So it reds on `main` alone, and this
branch did not introduce it.

`cargo run -- --known-red log_flush_retry` answers:

```
log_flush_retry: NOT ON THE LIST

  No measurement in this index has ever named it. That is not a claim that it is
  green — it is a claim that nobody wrote down a rate for it.
```

**It reds two ways, which is why this is not a scheduling classification.** The
wide run's assertion and the alone re-run's assertion are different in one of
the two red runs, and the harness says so itself:

```
  ALONE log_flush_retry: red again on a DIFFERENT failure — it failed twice, on two assertions, so this is not one defect reproduced and the divergence is itself the finding.
      wide:  the deadman never declared the volume failed:
```

and reproduces identically alone in the other:

```
  ALONE log_flush_retry: red again, the same failure both times — the defect is real. the deadman never declared the volume failed:
```

The two assertions are the test's `[hung]` and `[deadman]` arms:

```
FAIL log_flush_retry: no "transport broke on SCSI" in the log, so the staged hung device never met its recovery:
FAIL log_flush_retry: the deadman never declared the volume failed:
```

Both red boots reach `log-volume: partition mounted` and then either
`logd: cannot create /log/<stamp>.log: other error` or
`logd: /log has not answered (the sync: other error)`, which is the arm's own
staged refusal arriving where the assertion did not expect it — the guest is
alive and answering, not wedged.

The `[retry]` arm was green in every run seen, red or not:
`fsync: /log/<stamp>.log durable on attempt 2 after 29ms` and
`volume kept, checker silent, 41097 blob bytes byte-identical after a
once-refused flush`.

**Exit condition.** A `src/redlist.rs` row carries this name with the measured
rate rather than a `Seen`, or the arm is fixed at its owner so the name is green
at whatever rate a session can measure. `ALONE: GREEN` is not available as the
answer here: two of the three alone re-runs in the A/B above were red, one of
them on a different assertion than the wide run.

**A third way, measured 2026-09-07 on the same host while gating the metal-suite
driver work.** Beside the two assertions above it also reds as a boot that never
came up at all:

```
log_flush_retry: [qemu] Boot timed out waiting for ===READY===; the console carried:
```

Three points, each run alone: `main` 71ed50cf red, `metal-suite` 7f16914d red,
`metal-suite` f63bce1d red — so it predates that branch and the USB-shutdown
change on it. That the same name now reds in three different places, one of them
before userland, is against reading any of them as the arm's own staged refusal
arriving early.

## A third way, red on `main` both times it ran

`origin/main` at `7a5a98d3`, in a worktree of its own, one `--nightly
log_flush_retry` run: red, and the harness's re-run red again, on the volume
checker —

    /2026-09-24-210445.log: DIR_FileSize is 77758 bytes, which needs 152 clusters, and the chain holds 159

— clusters chained past the file's size, with `FSI_Free_Count` off by the
same kind of amount in the logd branch's runs. The logd branch at `583f21e5`
was red the same way in both of its runs, each re-run red too; at `eef19bd1`
it had been green. One
`main` run is not a rate, but it is this sentence on a tree without the
branch.

## A fourth way: the hung boot takes the disk before soundd is paged in

The `[hung]` boot (`usb-transport-break` + `usb-reset-break`) reds as

    [qemu] Init process crashed during boot:
    SEGFAULT tid=0: execute unmapped address at 0x10000056b7c  _start+0x0
    exit: soundd pid=5 code=-1

followed by `usb-storage: read of 1 blocks at … failed on disk 0` for the
root: the first WRITE(10) broke the transport, the reset ladder took the disk
offline, and soundd's first page was never read. The `[retry]` and `[deadman]`
arms were green in every one of these runs. Measured in one session on the dev
host, sequentially, while gating #510 (`toyos-fat32`'s refused-write repair;
its kernel diff is `fat32_adapter.rs`'s logging of a pending repair): that
branch's `b5eaed62` sources for `kernel/src/fat32_adapter.rs` and
`toyos-fat32/src`, 2 red of 4; its `5c73eca0`, 4 red of 4. Each red's alone
re-run was red on the same assertion. The same failure on the base arm puts
it before this branch; four runs a side do not separate the two rates.

### Root cause of the fourth way: the arm breaks the disk root is paged from

The `[hung]` boot's image carries ROOT, the ESP and `/log` on the one USB stick
(`gpt: device 16 carries the ROOT candidate … at LBA 71680+1392640` and `…the
log partition … at LBA 1464320+69632`), and `usb-transport-break` breaks *the
boot's first WRITE(10)* on whatever disk takes it. That write is logd creating
`/log/<stamp>.log`, which runs while init is still spawning soundd and
test-runner, whose text is demand-paged off that same stick. Once the ladder
takes the disk offline, the next page-in of either is refused
(`kernel/src/process.rs:1387`, `fault: … is backed by a file byte … that the
device would not read`) and the process dies as a `SEGFAULT` at `_start`
(`kernel/src/arch/idt/exceptions.rs:213`); before the ready marker, that is
`Died::Faulted` and `tests/common/qemu.rs:4684` ends the boot. From a red boot
on #510's head, `--nocapture`:

    [kernel 0.491 cpu0] spawn: /system/bin/test-runner pid=6 … entry=0x100000215fc
    [kernel 0.499 cpu0] usb-storage: 00:02.0 slot 1 transport broke on SCSI 0x2a: a staged break skipped the data phase wait; break 1 of 3 running
    [kernel 0.551 cpu0] usb-storage: 00:02.0 slot 1 is offline: …
    [kernel 0.551 cpu1] root: read of block 2032 failed
    [kernel 0.552 cpu1] fault: 0x100000215fc is backed by a file byte 348160 that the device would not read; leaving the fault unhandled
    [kernel 0.552 cpu0] log-volume: create of 2026-09-26-043306.log: device I/O failed
    [kernel 0.552 cpu1] SEGFAULT tid=0: execute unmapped address at 0x100000215fc

So the arm is green only when the break lands after soundd's and
test-runner's pages are resident. Across both trees, every hung boot whose
break came at or before 0.500 s was red and every one at or after 0.513 s was
green. Whichever of the two had not paged in yet is the one that dies: soundd
when the break is near 0.455 s, test-runner near 0.50 s.

**Not #510.** On the same host in one session, run one guest at a time,
`--nightly log_flush_retry --nocapture`: `main` `fb7bc2aa` 5 red of 6 runs
(10 of 11 hung boots red), #510 `4d4ad4df` 4 red of 4 runs (5 of 8 hung
boots red). Every red had the mechanism above. #510 moves which block the
create writes first (183175 against `main`'s 185991), not when the first
write goes out: the break came at 0.454–0.523 s on #510 and 0.455–0.513 s on
`main`.

**The harness verdict is right; the arm's premise is not.** A process did
die. What cannot hold is the arm's claim that a boot keeps its userland while
the disk it pages from goes offline. **Fix at the owner, the arm in
`tests/common/volumes.rs` (`log_flush_retry`, boot 3 from line 3007):** give
the hung boot its ROOT on a second disk, which the kernel accepts because it
selects ROOT candidates across every device (`kernel/src/gpt.rs:180`). `/log`
has to stay on the boot stick (`gpt.rs:435`, `locate_log`), so the first
WRITE(10), logd's, still breaks that stick, and nothing is paged from it.
Arming the break later cannot fix this, because logd's create comes before
the ready marker.
