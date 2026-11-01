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

**A large program is spawned from scattered frames.** A program a file server
holds is spawned from a copy in one memory object, which `SYS_SHM_CREATE` now
makes of 4 KiB frames in any order (`kernel/src/object/shm.rs`), so the hosted
clang's 148 MB image no longer needs one physical run; `hosted_clang_hello`,
which was refused `ENOMEM` linking, is the check. What is still one physical
run is a device-lendable object (`SHM_DEVICE`, `SharedMemObject::create_run`)
and each `dlopen` image (`issues/a-dlopen-image-is-one-contiguous-run.md`).

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

**Device windows are 4 KiB leaves**: a claimed function's BAR is an aperture
object mapped in the pages it covers (P1 of `issues/there-is-no-wifi.md`).

## Stages

The owner approved starting this work ("Start it") and delegated every
kernel-interface decision to the orchestrator, whose stages and rulings
follow. **Commit is strict**, by the orchestrator's ruling: a mapping
reserves its frame count when it is made and is refused by name then, never
killed at a fault. No promotion (the kernel has no thread to do it) and no
live split of a user leaf.

1. **Chunked user copies** owe one measurement: what a copy costs now that
   each 16 pages take one round trip of the space's lock, an inventory
   lookup and a table walk per page. No job measures it in-guest yet: the
   stage owes `copy_cost`, a job beside `syscall_cost` that times `read` into
   a 64 KiB and a 64 MiB window, fresh and warm, and its metal row, so that
   `cargo test --test toyos-build -- --metal copy_cost` at a change and at
   its base is the A/B.
2. **Superframe frame allocator.** Built (`kernel::frames`): 4 KiB frames
   carved from 2 MiB superframes, pins per frame, a frame freed while pinned
   deferred to its last unpin, page tables in frames of their own, and every
   2 MiB caller of the PMM unchanged. Promises, below, refuse a commitment
   before the machine runs dry.
3. **4 KiB user leaves.** Built: one inventory per space (`kernel::vm`,
   host-tested) is the only record of its ranges, and a software owned bit in
   the leaf the only record of a private frame; stacks are populated as they
   are touched, TLS is a page-sized object, and a typed syscall value across
   a page is copied in pieces (`abuse_page_straddle`'s delivery verdicts). A
   space holds at most `MAX_RANGES` ranges, refused past it, until a range's
   kernel memory is charged to its process.
   Owed: a process's floor on the T14 against the 10 to 14 MB above, and the
   hot paths' costs in `issues/user-memory-hot-paths-are-slower-than-2-mib-pages.md`.
4. **Demand-zero anonymous `mmap`.** Built, over 4 KiB pages only: a fault
   populates one page, unmapping a range no thread touched sends no IPI, and
   `munmap` and a `FIXED` `mmap` take any run of pages, as POSIX does.
   `vm_reserve`, `vm_protect`, `vm_discard`, `vm_decommit`, `vm_remap` and
   `vm_release` are the calls, behind libc's `mmap`, `mprotect`, `madvise` and
   `munmap`, std's and libc's allocators, which trim and grow a large block
   by `vm_remap` in place or by moving its pages, and std's thread stacks, each reserved
   at the size asked with a guard page below. A commitment is promised its
   frames when it is made, and every other allocation is refused rather than
   take a promised frame (`kernel::frames`), so a commitment past what memory
   holds is refused at the call, not at a fault; its page tables are taken
   then too. Kernel objects a process makes are charged to its job
   (`kernel/src/mm/charge.rs`).
5. **Shared text.** A spawn copies each page of its program's segments into
   the process at the page's first touch, out of the image object or out of
   a snapshot of its `/tmp` file; no fault reads a file. A ROOT program's
   image object is SYSTEM's subrange of ROOT's own pages (`fs::IMAGE`), so
   no spawn copies the whole binary first. Mapping ROOT's read-only text
   into each process without the per-page copy is the next step. Exit: two
   processes of one binary hold its text once.
6. **`/apps` image sharing.** The design is the owner's decision, owed when
   this stage is reached.
7. **Large user leaves where they pay.** No user leaf is 2 MiB now, so every
   user page costs a 4 KiB TLB entry and a table walk the 2 MiB windows did
   not: a binary's text and data, a heap, a stack, a large buffer. Unpriced:
   nothing measures the TLB misses of a workload against 2 MiB leaves. Exit:
   whole aligned spans of one range get a 2 MiB leaf where a measurement
   shows it pays, or the measurement shows it does not.

**Paged objects** (memory design §7, `issues/toyos-builds-itself.md` item 13)
are split in two. **7a**, delivered by the storage track's file-server work:
a pager's objects, read through `SYS_PAGED_READ` and never mapped
(`kernel/src/object/paged.rs`). **7b** stays this track's: mappings of them,
faults that park, their page tables and reverse records, holders' kernel
writes and their admission, after stage 4's seal.
