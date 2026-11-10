---
status: open
kind: defect
opened: 2026-09-29
---

# No user address is drawn per spawn

Every spawn places its image, its stack and every mapping after them by one
deterministic rule, the smallest free range that fits, from the space's
floor up (`kernel::vm::Inventory::place`, `kernel/src/mm/space.rs`), so an
address one process leaks is the same address in the next. Linux at `Ubuntu-6.8.0-142.142`
draws 32 bits for the image and the mmap base (`CONFIG_ARCH_MMAP_RND_BITS`,
`debian.master/config/annotations:18`) and 22 for the stack.

**Exit**: on every proving machine, over 256 spawns each claimed bit of the
three bases is set 88 to 168 times, and each base claims Linux's bits or its
shortfall is filed as a defect. **Mutation**: `BASE + n * 2 MiB`; a constant
seed. **Oracle**: Linux's bits.

A fourth address is constant since #1200: the startup block
(`toyos::startup::AT`), below the image. It is the only one of the four whose
bytes another party chooses: writable, up to 2 MiB, written by the parent,
and in a launched program holding strings of a caller with less authority than
the row. **Exit**: the block has no address of its own. It moves with the
image, found by every SDK copy through the process's module list
(`toyos::tls`), and the exit above counts its base with the other three.
