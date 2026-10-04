---
status: open
kind: defect
opened: 2026-10-04
---

# A boot paint at `subsystems ready` stalled 4.3 ms on the T14

One T14 boot of `lantalkcase`, at `43cbe73d4` (pull request #710's head, run
on 2026-10-04), wrote `panel: paints=10 px=8513536 us=25527 max_us=8194`. The
machine's record for that boot is `boot.lantalkcase.panel_max_us = 3919`
(`tests/metal/lenovo-20w0003amz.toml`), and the judge's ceiling 7838, so the
judge exited 1. The same kernel's four other boots of that run read `max_us`
3921, 3802, 3878 and 3870, and the negative control's four 3837, 3883, 3927
and 3853.

The extra time is one stall, not slower painting: the pixel count matches the
other boots (8513536 against 8496384..8518016), and the total paint time is
about 4 ms over theirs (25527 µs against 20828..21906), all of it in the one
maximum. It sits at the `Boot: subsystems ready` checkpoint's paint: on that
boot the next record after `subsystems ready` (0.244 s) is at 0.253 s and the
xHCI's first remapping entry at 0.254 s, where every other boot of both
kernels reads 0.248..0.249 s for the one and 0.249 s for the other; `storage
ready` is late by as much (0.740 s against 0.734..0.736).

The review of #710 read it as not that change's
(https://github.com/ToyOSOrg/ToyOS/pull/710#issuecomment-5976306774): every
panel paint is at a boot checkpoint, the last at `Boot: complete`, before the
I219's claim; and nothing the change adds runs between `peripherals ready` and
the claim. What stalled a paint for 4.3 ms is not known — a single boot, with
no instrument inside the paint.

Owner: the orchestrator, who holds the T14 record. Exit: the stall's cause is
named from a measurement — the review's alternating `lantalkcase` boots of
`e81d26db8` and `43cbe73d4`, three each, comparing `max_us` and the gap from
`subsystems ready` to the next record, is the first — and removed, and three
`lantalkcase` boots then read `panel_max_us` inside the record's ceiling.
