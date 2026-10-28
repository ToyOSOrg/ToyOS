---
status: open
kind: defect
opened: 2026-10-28
---

# The `counters` row's idle second can hold the EC's second boot query

`counters_metal` starts its idle second once acpiserver has said it took
the embedded controller's first query (`acpi_said` in
`tests/guest/rust-tests/src/bin/counters_metal.rs`), and the row reds any
line stamped inside that second. On the T14 the controller raises 0x4f near
boot, and in some boots 0x40 within about a second of it, in either order
(`r3/prior-testcases.txt` in `ToyOSOrg/evidence` branch `pr-1084`). In the
32 earlier testcases boots listed there, the idle second began after every
query of the boot's first 23 s. #1084's T14 testcases boot at `7c9cf910f`
carried two jobs (`r3/`). Its first query was 0x40 at 12.520 s, its idle
second began 152 ms later at 12.672 s, and 0x4f was taken for the first
time at 13.538 s, inside it:
`FAIL counters: the idle second ... holds lines`.

The machine was not quiet, which the row is right to refuse. But the row's
premise, that the first query ends the controller's boot queries, does not
hold. Its verdict depends on how long the boot's other work delays the
second, which passing only in large images hides.

Owner: the `counters` row (`counters_on_metal` in `tests/toyos.rs`, and
`counters_metal`).

**Exit**: the idle second begins only once the controller's boot queries
are over, by a rule that does not depend on the image's other jobs, and
the `r3/` boot is refused as not quiet before its idle second, rather than
red inside it.
