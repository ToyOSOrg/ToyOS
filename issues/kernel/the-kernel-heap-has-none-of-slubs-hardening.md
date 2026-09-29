---
status: open
kind: defect
opened: 2026-09-29
---

# The kernel heap has none of SLUB's hardening

The kernel heap is `dlmalloc` 0.2.13 (`kernel/src/mm/alloc.rs:500,513`). A
free chunk carries its list links as plain pointers (`src/dlmalloc.rs:62-67`),
and unlinking one writes through both without checking that either points
back (`src/dlmalloc.rs:1118-1131`), so one heap overflow into a free chunk is an
arbitrary write. Chunks come back in a fixed order, and every type of every
size shares one arena. Linux at `Ubuntu-6.8.0-142.142` sets three defaults
against this on the T14: `CONFIG_SLAB_FREELIST_HARDENED` stores each free
pointer XORed with a per-cache secret and its own address
(`mm/slub.c:479-490`), `CONFIG_SLAB_FREELIST_RANDOM` shuffles each slab's
free list (`mm/slub.c:2228,2290`), and `CONFIG_RANDOM_KMALLOC_CACHES` spreads
each size over 16 caches chosen by call site and a per-boot seed
(`include/linux/slab.h:340-341,398-401`).

**Exit**: a free-list link is stored encoded with a per-boot secret and
checked on every unlink, a size class's allocation order is drawn per boot,
and one size is spread over 16 caches by call site and a per-boot seed. Host
tests on the allocator: a corrupted link panics at the next unlink, and
storing it plain passes it and reds; two seeds give two allocation orders, and
a fixed order reds; 1000 call sites of one size use all 16 caches, and one
cache reds.
