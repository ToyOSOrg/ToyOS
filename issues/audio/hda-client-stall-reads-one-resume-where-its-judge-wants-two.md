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

## Measured once a job's stream is gone before the job exits

The `testcases` boot of the orchestrator's T14 `--metal hda_tone` run at
`1621eb5f0` (#641), from the tone's removal to the stall's end, in order:

```
{… 5.670 soundd} soundd: client 0 removed (closed)
{… 5.670 soundd} soundd: wakes=645 … clients=0 …
[… 5.670 cpu4] exit: test_rs_audio_tone pid=11 code=0 cpu=2ms
[… 5.672 cpu7] spawn: /system/bin/test_rs_hda_client_stall pid=12 …
{… 5.673 tid=1 soundd} soundd: opening stream: 44100Hz 2ch fmt=0
{… 5.673 soundd} soundd: client 0 connected (id=1)
{… 7.830 soundd} soundd: client 1 removed (closed)
{… 7.830 soundd} soundd: wakes=79 … clients=0 …
{… 7.854 soundd} soundd: suspended
{… 8.130 tid=1 soundd} soundd: opening stream: 44100Hz 2ch fmt=0
{… 8.131 soundd} soundd: client 0 connected (id=2)
{… 8.131 soundd} soundd: resumed
{… 9.040 soundd} soundd: client 2 removed (closed)
{… 9.040 soundd} soundd: wakes=465 … clients=0 …
{… 9.063 soundd} soundd: suspended
```

- The tone's stream is removed, and its session flushed, before its job
  exits; the stall's first stream opens 3 ms after that exit.
- soundd suspends 20 to 25 ms after the last removal: `7.830`→`7.854` and
  `9.040`→`9.063` here, `8.565`→`8.585` and `9.755`→`9.780` above.
- So the first stream opens on a soundd that has not suspended, which is not
  the premise `client_stall_on_metal` asks of it. The judge reads this log as:

```
FAIL hda_client_stall: soundd resumed 1 time(s) — the second stream did not find a suspended daemon, so nothing here tests a resume:
```

## Exit condition

`hda_client_stall` PASS on the orchestrator's T14 run of the head that lands
the fix, and this file is deleted.
