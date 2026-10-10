---
status: open
kind: defect
opened: 2026-10-10
---

# The boot volume's server holds a claim that writes

On a disk the kernel drives — the boot stick, until usbd serves it — the
supervisor endows the boot volume's file server a partition claim on the
running slot's volume (`storage_endowment`, `userland/supervisor/src/main.rs`).
A claim carries `Rights::WRITE` and no `DUP` (`initial_rights`,
`kernel/src/object/ops.rs`), so nothing can hand that server a duplicate
narrowed to reading: the volume the loader reads the kernel from stays
unwritten only by that server's promise to mount it read-only. On a disk the
block service serves, the same server's grant does not write, and diskserver
answers a write through it `ReadOnly` (`block_grants_reach_their_partitions`).

**Exit**: the boot volume's server on the boot stick holds a partition it
cannot write — a read-only session once usbd serves the stick, or a claim the
kernel mints without `WRITE` — and a test's write through it is refused.

## Owner

The usbd cutover of `issues/the-kernel-is-small-interrupts-post-and-threads-wait.md`; unheld.
