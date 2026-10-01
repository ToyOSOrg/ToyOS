---
status: expected-red
kind: tooling
opened: 2026-10-01
---

# A ready marker read off the 16550 file can end the boot wait mid-line

`638L-638r5-whole.log` (`wt/toyos-tight` `59940c452`, "ceilings paid at
1.00x"): `root_candidate_malformed` — "the loader did not refuse a ROOT its
signature does not cover: Slot A: REFUSED, its root is". The loader's line is
"Slot A: REFUSED, its root is not the bytes its signed header names"
(`648-648-whole.log`, the same test passing), so the capture holds the first
half of it.

`wait_for_ready` (`tests/common/qemu.rs`), for a marker other than the
default, looks for it in the 16550's log file once a second, and that file is
QEMU's, written a byte at a time while the loader prints. `ROOT_REFUSED` is
"Slot A: REFUSED, ", so a read between those bytes and the rest of the line
ends the wait, and `boot_expecting_root_refusal` hands back a line cut short.
Every test booted to a 16550 marker can read one; this is the one that has.
The `aio failed: Input/output error` beside it in the log is
`root_chunk_refused`'s injected bad sector, not this test's.

**Exit**: the boot wait ends on a whole line; then the row goes.
