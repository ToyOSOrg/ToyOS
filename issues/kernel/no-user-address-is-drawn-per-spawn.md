---
status: open
kind: defect
opened: 2026-09-29
---

# No user address is drawn per spawn

Every spawn puts the image at `USER_VM_BASE` (`kernel/src/loader/mod.rs:48`)
and the stack at `STACK_BASE` (`kernel/src/vma.rs:12`), and allocates
mappings in one fixed window (`kernel/src/vma.rs:18`), so an address one
process leaks is the same address in the next. Linux at `Ubuntu-6.8.0-142.142`
draws 32 bits for the image and the mmap base (`CONFIG_ARCH_MMAP_RND_BITS`,
`debian.master/config/annotations:18`) and 22 for the stack.

**Exit**: on every proving machine, over 256 spawns each claimed bit of the
three bases is set 88 to 168 times, and each base claims Linux's bits or its
shortfall is filed as a defect. **Mutation**: `BASE + n * 2 MiB`; a constant
seed. **Oracle**: Linux's bits.
