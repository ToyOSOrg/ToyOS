---
status: open
kind: defect
opened: 2026-09-29
---

# `hda_tone` reads underruns on the T14 where its judge wants none

The T14 run of `wt/toyos-metaljudges` at `f5c80264f` reds the row with:

```
FAIL hda_tone: soundd filled 104 period(s) the tone client had not covered, on a client that keeps its ring full:
```

The job itself exits 0 and prints `tone done`. Its cause is not measured;
whether the judge or soundd is wrong is open.

The T14 run of `main` at `7e151819` reddened the row earlier, on `the
testcases's log carried no kernel output at all`.

## Measured

The `testcases` boot's log over the window the judge printed, selected lines in
order:

```
{… 3.021 tid=1 soundd} soundd: opening stream: 44100Hz 2ch fmt=0
{… 3.021 soundd} soundd: client 0 connected (id=0)
{… 3.021 soundd} soundd: resumed
{… 5.024 soundd} soundd: wakes=984 completions=689 submitted=697 underruns=0 drains=0 … clients=1 …
[… 6.252 cpu0 tid=1] exit: test_rs_audio_tone tid=1 code=0 cpu=12ms (after 48 like it suppressed)
{… 6.252 pid=8 test-runner} tone done
{… 6.252 test-runner} ===TEST_END test_rs_audio_tone exit=0===
{… 6.252 test-runner} ===TEST_START test_rs_hda_client_stall===
[… 6.253 cpu7] spawn: /system/bin/test_rs_hda_client_stall pid=9 …
{… 6.254 tid=1 soundd} soundd: opening stream: 44100Hz 2ch fmt=0
{… 6.254 soundd} soundd: client 1 connected (id=1)
{… 6.258 soundd} soundd: client 0 removed (closed)
{… 7.024 soundd} soundd: wakes=989 completions=688 submitted=688 underruns=39 drains=0 … clients=1 …
{… 8.412 soundd} soundd: client 1 removed (closed)
{… 8.418 soundd} soundd: wakes=741 completions=479 submitted=479 underruns=65 drains=0 … clients=0 …
```

The printed window carries three stats lines, with `underruns=0`, `underruns=39`
and `underruns=65`, which sum to the 104 the row names.

## Exit condition

`hda_tone` PASS on the orchestrator's T14 run of the head that lands the fix,
and this file is deleted.
