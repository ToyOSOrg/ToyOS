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
