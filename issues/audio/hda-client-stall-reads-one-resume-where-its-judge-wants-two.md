---
status: open
kind: defect
opened: 2026-09-29
---

# `hda_client_stall` reads one resume on the T14 where its judge wants two

The T14 run of `wt/toyos-metaljudges` at `6737a442` reds the row with:

```
FAIL hda_client_stall: soundd resumed 0 time(s) — the second stream did not find a suspended daemon, so nothing here tests a resume:
```

The job itself exits 0 and prints `stalled 8 then 2 times, soundd survived`.
Its cause is not measured; whether the judge or soundd is wrong is open.

## Measured

The `testcases` boot's log, from the job's spawn to its end, in order:

```
{… 6.275 soundd} soundd: client 0 removed (closed)
[… 6.276 cpu6] spawn: /system/bin/test_rs_hda_client_stall pid=9 …
{… 6.276 soundd} soundd: wakes=662 … clients=0 …
{… 6.333 tid=1 soundd} soundd: opening stream: 44100Hz 2ch fmt=0
{… 6.333 soundd} soundd: client 0 connected (id=1)
{… 8.565 soundd} soundd: client 1 removed (closed)
{… 8.565 soundd} soundd: wakes=126 … clients=0 …
{… 8.585 soundd} soundd: suspended
{… 8.836 tid=1 soundd} soundd: opening stream: 44100Hz 2ch fmt=0
{… 8.836 soundd} soundd: client 0 connected (id=2)
{… 8.836 soundd} soundd: resumed
{… 9.755 soundd} soundd: wakes=480 … clients=0 …
{… 9.780 soundd} soundd: suspended
```

- The window `tests/common/audio.rs`'s `job_window` cut for two sessions ends
  at the `8.565` stats line: the `clients=0` stats line at `6.276`, after the
  spawn and before the first stream opens, is counted as the first session's
  end. The printed window carries no `soundd: resumed`.
- Across the job's whole span the log carries one `soundd: resumed`, at the
  second stream. The first stream opens with no `soundd: suspended` between
  the previous client's removal at `6.275` and its open at `6.333`, so
  `client_stall_on_metal`'s `resumes < 2` reds on that span too.

## Exit condition

`hda_client_stall` PASS on the orchestrator's T14 run of the head that lands
the fix, and this file is deleted.
