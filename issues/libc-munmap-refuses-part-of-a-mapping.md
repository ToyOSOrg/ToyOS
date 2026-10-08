---
status: open
kind: defect
opened: 2026-10-08
---

# libc's `munmap` refuses part of a mapping, which POSIX unmaps

POSIX's `munmap(addr, len)` removes every whole page that holds a byte of
`[addr, addr + len)`, whatever mappings those pages belong to, and answers 0
for a range that holds none. `userland/libc/src/posix_io.rs`'s `munmap`
unmaps one whole mapping or nothing:

- `addr` starts a mapping and `len`, in the 4096-byte pages
  `sysconf(_SC_PAGESIZE)` answers, is every page of it: 0, and the mapping is
  gone. A `len` that ends anywhere in the mapping's last page is that.
- Anything else is -1 with `EINVAL` and unmaps nothing: the first pages of a
  mapping, its last pages, pages in its middle, a range over two mappings, a
  range that holds no mapping, and no bytes.

So a C program that trims a mapping — an allocator giving back a tail, a
guard page cut from a stack, an aligned block cut from a larger one — is
told `EINVAL` and keeps all of what it mapped. Nothing it kept is taken.

The kernel is why: a mapping is one `vma::Region` over one `PageAlloc`, with
no part to take away, and `SYS_MUNMAP` takes only the length the mapping's
`mmap` was asked for (`kernel/src/syscall/vm.rs`). That is the ruling in
`issues/process-memory-is-2-mib-pages-and-that-caps-the-process-count.md`'s
stage 4.

## Evidence

`tests/testcases/tinycc/206_libc_refusals.c` maps two pages and reads the
answer, `errno` and both pages for the first page alone, the second alone,
no bytes and a byte past both; its `.expect` holds `-1, EINVAL` for each and
both pages' bytes after them. The lengths that name a mapping are
`memreq::whole_pages`, host-tested in `tests/libc-arch/src/memory_refusals.rs`.
No program in the tree trims a mapping: read, across `userland/` and the C
corpus, and not across what a user builds against this libc.

## Exit condition

`206_libc_refusals` reads 0 for the first of two pages, the second page still
holds its byte, and a touch of the first faults; or the owner rules the
refusal final and this file becomes `kind: rejected`.

## Owner

The orchestrator, who holds the 2 MiB track and ruled the refusal: the first
exit needs a region that splits, which that track's stage 4 is where it would
be built.
