---
status: open
kind: defect
opened: 2026-10-21
---

# `tlb_shootdown_waits` reds on the T14, on main

On the T14, `tlb_shootdown_waits` (the `shared-debug` boot) panics in its
arming step, before any trial: with each of the 7 other CPUs' acknowledgement
held 20 ms late in turn (`SYS_DEBUG` `TLB_ACK_DELAY_ARM`), the cheapest of
those shootdowns cost the initiator microseconds.

```
panicked at src/bin/tlb_shootdown_waits.rs:74:5:
with each of the 7 other CPUs answering 20000000ns late in turn, the cheapest of those shootdowns cost the initiator 7426ns — it is not waiting for every other CPU
```

That is main at `fec5fdd02` (7426 ns, 2026-10-21), and `36570e1b1`, a branch
over main touching no TLB or shootdown path, reads the same (7345 ns). Each
boot read it once.

**What the log shows, and no more**: at least one of the 7 staged shootdowns
returned to its initiator without waiting out the delay staged on its CPU:
either that CPU was not among the shootdown's targets, or its acknowledgement
was not held. The log does not say which CPU, or which. The test's premise is
that every other CPU is a target (`tests/guest/rust-tests/src/bin/tlb_shootdown_waits.rs`'s
header); the kernel's shootdown paths changed under PCID on 2026-10-12
(`806130ce9`, `86877feb6`), which nothing here ties to it. When it last passed
on the T14 is not recorded here.

Owner: the kernel's TLB shootdown, `issues/a-tlb-shootdown-reaches-only-the-cpus-that-ran-the-space.md`'s track.

**Exit**: `tlb_shootdown_waits` green on the T14.
