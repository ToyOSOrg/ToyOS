---
status: open
kind: tooling
opened: 2026-08-08
---

# The HDA ring fix is unverified on the T14

Filed out of the `repeated completion for free buffer` entry when that closed in QEMU.

That QEMU close has no positive control either. The only thing in the tree that
can write the line is soundd's own `assert_eq!` (`userland/soundd/src/mix.rs`),
so `hda_client_stall`'s `must_not_say` for it is a second spelling of the
`must_be_clean()` beside it, and no arm anywhere provokes a double completion to
show the assert can see one. Closing that needs an actuator in the completion
loop, which is soundd's audio path.

QEMU's `intel-hda` and the T14's controller are different devices, and the fix
was measured on the first. What the next metal boot must show: `tone` playing to
completion with soundd alive, `deferred=0`, and `underruns` nonzero only if the
client stalls. Read `starve_max` on the same stats line
(`toyos-mixer/src/stats.rs`, printed by `userland/soundd/src/mix.rs`) beside
`underruns`: it is the longest unbroken run of underrun periods, so near 1 with
a large `underruns` is a client missing by a hair and 8–20 is a stall of one
whole 21–53 ms ring.

A `the engine completed 0x.., which is no walk of an 8-period ring` panic would
be **new**, and would be the first evidence that `SDnLPIB` is the wrong position
source there. The driver reads `SDnLPIB` and nothing else: the DMA position
buffer is deliberately out of scope, and the answer to an untrustworthy
`SDnLPIB` on the T14 is to say so and switch sources, never to carry both.
