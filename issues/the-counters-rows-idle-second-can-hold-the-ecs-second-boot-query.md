---
status: open
kind: defect
opened: 2026-10-28
---

# The `counters` row's idle second can hold the EC's second boot query

`counters_metal` starts its idle second once acpiserver has said it took
the embedded controller's first query (`acpi_said` in
`tests/guest/rust-tests/src/bin/counters_metal.rs`), and the row reds any
line stamped inside that second. On the T14 the controller raises more than
one query near boot. In every testcases readback on hand the first was 0x4f,
at 13 to 22 s, so the second began after it. In #1084's T14 testcases boot at
`7c9cf910f` (`ToyOSOrg/evidence` branch `pr-1084`, `r3/`) the first was 0x40
at 12.520 s. The second began at 12.672 s, and 0x4f was taken for the first
time at 13.538 s, inside it: `FAIL counters: the idle second ... holds lines`.
Query 0x40 also comes first in other T14 images' boots (benchcase,
cryptocase, netbenchcase readbacks).

The machine was not quiet, which the row is right to refuse. But the row's
premise, that the first query ends the controller's boot queries, does not
hold, so its verdict depends on which query the firmware raises first.

Owner: the `counters` row (`counters_on_metal` in `tests/toyos.rs`, and
`counters_metal`).

**Exit**: the idle second begins only once the controller's boot queries
are over, by a rule that holds for the `r3/` boot (0x40 then 0x4f), and that
readback judges green under it.
