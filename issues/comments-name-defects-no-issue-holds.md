---
status: open
kind: finding
opened: 2026-10-04
---

# Three comments name a defect that no issue file holds

Each of these cited an area directory, not a file, and flattening the tracker
dropped the pointer because no file under that area named the subject:

- `tests/toyos-rust-tests/src/bin/abuse_elf_loader.rs`, "the actuator for the
  allocator-lock defect": a >2 MiB `KernelAllocator::alloc` assert fires while
  the dlmalloc lock is held, and the comment says the defect "has stayed open".
  No issue names it.
- `kernel/pure/sched/cpu.rs`, `hand_off`: "the `BTreeMap`-inside-its-own-insert
  class", cited as a class the kernel area tracked.
- `kernel/loom/tests/reap_gate.rs`: the crash report's `try_lock` losing to
  `PROCESS_TABLE`, so a fault report printed a bare address.

**Exit condition:** each is either filed as a defect, if the tree still has it,
or its comment says it as an invariant and stops calling it a standing defect.
