---
status: open
kind: track
opened: 2026-09-08
---

# Process memory is in 4 KiB pages, and each process still copies its program's text

**Owner premise (2026-09-08):** ToyOS is a gold-standard OS for modern
hardware with no restrictions for modern hardware. "The right simplification
for this OS" was never the premise. A design that caps what modern hardware
can do is a restriction, and 2 MiB-only paging caps the process count.

**What is to be built:** process memory — stacks, thread-local blocks, heaps,
small data and code segments, and device windows handed to a process — moves
to 4 KiB pages. 2 MiB stays where it pays: large mappings, DMA grants, the
IOMMU's superpages, the kernel's own mappings. One paging design serves both,
on x86-64 and on the ARM64 the tree is kept portable for
(`issues/toyos-runs-on-arm64.md`).

**A large program spawned from a large parent is refused.** A program a file
server holds is spawned from a copy in one memory object, and `SYS_SHM_CREATE`
allocates that object physically contiguous (`kernel/src/object/shm.rs`,
`alloc_contiguous`): about 148 MB for the hosted clang. A small std parent
spawns `ld.lld` that way, but clang's driver, starting `cc1` and `ld.lld` from
`/apps/clang`, is refused `ResourceExhausted` (`ENOMEM` in libc), so
`hosted_clang_hello` compiles and cannot link. That the contiguous allocation
is the one refused is inferred: the errno is measured, the allocation is
not. Removing contiguous user memory is the exit's; this row is its check.

**The constraint that makes this a track, measured on the T14** (run 29
readback, `PMM: 100/16038MB used` at 11 s with four user processes and three
kernel threads live; the same record's rows: stack 16 pages held, init-tls 5,
elf 4, mmap 7, demand-page 4): a user process holds about 10 to 14 MB before
it does any work, one 2 MiB page each for its code and data, its thread-local
block, its first heap mapping and its stack, a second stack page as it grows.
A thousand such processes is the whole 16 GB machine. Packing a process's
small regions into one shared page lowers the floor to about 4 MB, since every
stack still needs its own page with an unmapped neighbour as its guard; that
moves the ceiling to a few thousand and not past it. x86-64 offers no page
size between 4 KiB and 2 MiB. That `PMM:` record and its rows went with the
kernel's idle report (the owner's ruling of 2026-10-04: "Delete the periodic
report and its counters"); stage 3's floor is read from the used memory
`SYS_SYSINFO` reports.

**Device windows are already 4 KiB leaves**: a claimed function's BAR is
mapped in place in the pages it covers (P1 of `issues/there-is-no-wifi.md`).
`map_range` maps any run of 4 KiB-aligned pages, a 2 MiB leaf wherever a whole
aligned frame lands on a 2 MiB boundary and a table of 4 KiB leaves for the
rest (`toyos_userbound::leaves`), and unmapping a slot of 4 KiB leaves frees
its table once no CPU can walk it.

## Stages

The owner approved starting this work ("Start it") and delegated every
kernel-interface decision to the orchestrator, whose stages and rulings
follow. **Commit is strict**, by the orchestrator's ruling: a mapping
reserves its frame count when it is made and is refused by name then, never
killed at a fault. No promotion (the kernel has no thread to do it) and no
live split of a user leaf.

1. **Scatter-gather user copies** (#593) owe one measurement: what the bulk
   window's `Vec<Segment>` costs under the kernel heap's lock. It is one
   heap allocation per bulk copy, taken before the address-space lock; the
   host A/B in #593's review put a 64 MiB `read` at 1.28–1.31× main's, 32
   to 40 ns more. No job measures it in-guest yet: the stage owes
   `copy_cost`, a job beside `syscall_cost` that times `read` into a 64 KiB
   and a 64 MiB window, and its metal row, so that
   `cargo test --test toyos-build -- --metal copy_cost` at the stage's merge
   and at its base is the A/B.
2. **Superframe frame allocator.** Built (`kernel::frames`): 4 KiB frames
   carved from 2 MiB superframes, pins per frame, a frame freed while pinned
   deferred to its last unpin, page tables in frames of their own, and every
   2 MiB caller of the PMM unchanged. The watermark is the ledger of
   `issues/toyos-has-its-own-allocator.md`'s migration, which refuses a
   commitment before the machine runs dry.
3. **4 KiB user leaves.** Built: one inventory per space (`kernel::vm`,
   host-tested) is the only record of its ranges, and a software owned bit in
   the leaf the only record of a private frame; stacks are populated as they
   are touched, TLS is a page-sized object, and a typed syscall value across
   a page is copied in pieces (`abuse_page_straddle`'s delivery verdicts).
   Owed: a process's floor on the T14 against the 10 to 14 MB above.
4. **Demand-zero anonymous `mmap`.** Built, over 4 KiB pages only: a fault
   populates one page, unmapping a range no thread touched sends no IPI, and
   `munmap` and a `FIXED` `mmap` take any run of pages, as POSIX does. What
   remains of the stage's exit — `mmap` past what memory can hold refused at
   `mmap` — is the strict charge of the allocator track's migration
   (`issues/toyos-has-its-own-allocator.md`).
5. **Shared text.** A spawn copies its program's segments into the process
   whole; no fault reads a file. ROOT's read-only text shared zero-copy is
   the memory design's stage 4 (immutable sealed objects). Exit: two
   processes of one binary hold its text once.
6. **`/apps` image sharing.** The design is the owner's decision, owed when
   this stage is reached.
7. **Large user leaves where they pay.** No user leaf is 2 MiB now; whether
   whole aligned spans get one again is the memory design's to decide.
