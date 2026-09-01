---
status: open
kind: defect
opened: 2026-09-01
---

# After a PS/2 queue overflow the console's input path can stop taking bytes for the rest of the boot

Staged on the dev host, 2026-09-01, on the **shipping kernel** with no actuator
armed: boot `console/system.toml`, wait for `console: ready`, then inject
`echo surface-up-zqjxk` — 21 unshifted characters, 42 set-1 bytes in three
bursts, plus a 2-byte Enter — back to back with no guest-side wait, and repeat
that ten times, sampling the decoded panel after each.

Nineteen of twenty boots behaved as a dropped byte should: the first line came
up short and every later one arrived whole. **One did not.**

```
EARLY 44-byte attempt0: row "/home/root> echo sur"; echo false
EARLY 44-byte attempt1: row "/home/root> echo sur"; echo false
...
EARLY 44-byte attempt9: row "/home/root> echo sur"; echo false; guest said 0 bytes
```

The panel stopped at eight characters — sixteen set-1 bytes, exactly one
device queue — and never moved again. Four hundred and forty bytes were
injected over the ten attempts and the guest took none of them: at the last
attempt it emitted nothing at all on its console (`guest said 0 bytes`). It was
not dead; earlier attempts in the same boot had produced output.

**This is the shape of the sightings, and a dropped byte is not.** A queue that
merely overflows loses the tail of one line and takes the next one; an input
path that stops explains `10 typed lines and none of them came back`, which is
what `console_locale_detect` and `desktop_locale_detect` reported on CI on
2026-08-31.

**What is not established: which side stopped.** The kernel's own drain counter
would separate "the ISR stopped taking bytes off the controller" from "the
console stopped reading the kernel's queue", and that counter needs
`i8042-trace`. Twenty boots with the trace armed did not reproduce the wedge at
all: every one of those 200 attempts recovered by the next line and none ever
read `drained 0`. So the arm that can see the counter is the arm that does not
fail, and the instrument changes the outcome — arming the trace also arms
`i8042-fast-health` and `i8042-edge-race` (`kernel/src/actuator.rs`'s `IMPLIES`)
and selects the test kernel, so it is a different guest in three ways.

Exit: reproduce the wedge with a byte counter visible — a counter the shipping
kernel already prints, or an actuator that adds the count and nothing else —
and then read whether `RX_BYTES` is still rising while the panel is frozen. That
one number says whether this is a driver defect or a console one.

Not a reason to leave `shell_type_once` unpaced: pacing keeps the harness from
provoking this, and the tracker keeps the defect.
