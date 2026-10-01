---
status: open
kind: tooling
opened: 2026-10-01
---

# A ready marker read off the 16550 file can end the boot wait mid-line

`638L-638r5-whole.log` (`wt/toyos-tight` `59940c452`, "ceilings paid at 1.00x"):
`root_candidate_malformed` failed with "the loader did not refuse a ROOT its signature does not
cover: Slot A: REFUSED, its root is". The loader's line is "Slot A: REFUSED, its root is not the
bytes its signed header names" (`648-648-whole.log`, the same test passing), so the capture held
the first half of it.

`wait_for_ready` (`tests/common/qemu.rs`), waiting for a marker other than the default, looks for
it in the 16550's log file once a second, and QEMU writes that file a byte at a time as the guest
prints. `ROOT_REFUSED` was "Slot A: REFUSED, ", so a read between those bytes and the rest of the
line ended the wait with the line cut short. `root_candidate_malformed` has left the guest suite
for a host test in `toyos-rootimage`; every guest test that waits on a 16550 marker still reads
the file the same way.

Owner: the orchestrator.

**Exit**: the boot wait ends on a whole line of the 16550 file, and a `tests/checks` case that
grows that file a byte at a time past a marker reds when the wait returns before the line's
newline.
