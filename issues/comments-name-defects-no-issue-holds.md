---
status: open
kind: finding
opened: 2026-10-04
---

# A comment names a defect that no issue file holds

It cited an area directory, not a file, and flattening the tracker dropped the
pointer because no file under that area named the subject:

- `tests/toyos-rust-tests/src/bin/abuse_elf_loader.rs`, "the actuator for the
  allocator-lock defect": a >2 MiB `KernelAllocator::alloc` assert fires while
  the dlmalloc lock is held, and the comment says the defect "has stayed open".
  No issue names it.

**Exit condition:** it is either filed as a defect, if the tree still has it,
or its comment says it as an invariant and stops calling it a standing defect.
