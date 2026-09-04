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

## Which side stopped, and the two armed measurements that disagree

The kernel's drain counter separates "the ISR stopped taking bytes off the
controller" from "the console stopped reading the kernel's queue", and it needs
`i8042-trace`.

- **Twenty armed boots here did not reproduce the wedge at all**: 200 attempts,
  every one recovered by the next line, and `drained 0` never once appeared.
- **An independent staging of the same arm reported it 2 of 5**, with
  `injected 44, kernel drained 16` and then attempts 1 through 9 all
  `drained 0`, the row frozen at `echo sur`. That result is reported here, not
  measured here.
- **Seen once on the dev host under load**, unarmed, on the `iommu-domains`
  branch: `console_locale_detect` STALLED with "waiting for the wizard to ask
  for a key under /system/bin/console — the console did not lend it the keyboard — it
  never stopped talking and never got there", in a run whose 1-minute load was
  9.7 with two other worktrees holding guest slots and which took 1833 s against
  198 s for the same tree alone. The harness called it a blown liveness guard;
  it was `ALONE: GREEN`, and green in a 198 s whole-suite run of the same commit.
  A sighting outside CI and outside the armed arm, recorded because nothing else has one.
- **Seen again on the dev host under load, 2026-09-04, and this one has a
  denominator**: the same STALL sentence, **891 s**, `ALONE: GREEN — it fails
  only beside other guests`, in **1 of 6** full `cargo test` fast tiers run in
  one worktree that day, each with single-test runs of the same suite beside
  it. Green in the three nightly `ci` runs of the same week. `src/redlist.rs`
  carries it as this name's first `DevHostLoaded` row.

The two do not reconcile at a common rate: at the 2-of-5 arm's own p = 0.4,
P(0 of 20) is 3.66e-05. **The tree is not the difference** — the branch's
armed boots ran with `4c35c920` as their base, which is the same base the other
staging used, so the merge that followed cannot account for it. Nobody has an
explanation yet, and the file says so rather than asserting the negative.

**If the armed arm can wedge, this is nearly answered.** The counter is then
available on a wedged boot, and the reported `drained 0` *while bytes were
still being injected* is the ISR side having stopped taking them off the
controller — not the console having stopped reading the kernel's queue.

Exit: reproduce a wedge with the counter visible and read whether `RX_BYTES` is
still rising while the panel is frozen. One number decides it, and it is
already within reach of the armed arm rather than blocked on a new instrument.

Not a reason to leave `shell_type_once` unpaced: pacing keeps the harness from
provoking this, and the tracker keeps the defect.
