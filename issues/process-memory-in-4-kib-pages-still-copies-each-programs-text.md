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
run is a device-lendable object (`SHM_DEVICE`, `SharedMemObject::create_run`);
a library is laid out in the process's own private memory by the SDK's
loader (`toyos::dl`).

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
5. **Shared text.** Built for an image nobody can write: a ROOT program's
   image object is SYSTEM's subrange of ROOT's own pages (`fs::IMAGE`), and
   a spawn and the library loader map its read-only segments from it
   (`SpawnBacking::Share`, `vm_map`), copying only a writable segment, a page
   at its first touch. A program anywhere else is still read whole into
   memory of the spawner's and copied a page at a time, until 7b's step 3.
   A shared segment's mapping is never widened, so a program can no longer
   make its own read-only segment writable (`mprotect`). Exit for that: 7b's
   step 2 converts on protect, so a protect that adds write to such a range
   commits its pages and makes it a copy range of the same object; only a
   page shared until its first write waits for step 5's write faults. Exit:
   two processes of one binary hold its text once, wherever it is served
   from.
6. **`/apps` image sharing.** Decided by the orchestrator under the owner's
   delegation of technical decisions (2026-11-11): 7b's step 3 maps images
   from every pager-backed volume, `/apps`, `/tmp` and `/home` alike, sealed
   against writes in place while they run. When a file server ends, every
   mapping of its objects is revoked (7b's D1), so a program running from
   DATA, like any process reading a mapping of a DATA file, dies at its next
   access to that mapping.
7. **Large user leaves where they pay.** No user leaf is 2 MiB now, so every
   user page costs a 4 KiB TLB entry and a table walk the 2 MiB windows did
   not: a binary's text and data, a heap, a stack, a large buffer. Unpriced:
   nothing measures the TLB misses of a workload against 2 MiB leaves. Exit:
   whole aligned spans of one range get a 2 MiB leaf where a measurement
   shows it pays, or the measurement shows it does not.

**7b lands in steps**, each a pull request: (1) a range of a memory object
mapped where the caller says, an immutable one shared and executable, which
is stage 5 for `/system`: built; (2) a paged object mapped read-only and
shared: faults that park on the pager, reverse records, revocation; (3) a
spawn maps its image from any pager-backed volume (stage 6), sealed against
writes in place while it runs; (4) the
storage track's attributes and names without a request; (5) writable shared
mappings and write-back; (6) the #1077 retry
(`issues/a-tcp-connect-zeroes-twelve-mebibytes.md`).

- **Owed on metal:** step 1 (#1259), on the T14 and the AMD bench: the whole
  `testcases` boot, whose `vm_map_shares` member is the x86-64 verdict that a
  program's read-only segments are its image's pages and take no private
  frame (`spawn_cost` among its other members), and the `shared-debug` and
  `proctreecase` boots. Main must show every job of each boot exit 0.
- **Owed on metal:** step 1 (#1259), on the T14 and the AMD bench: osbench's
  `process.spawn_wait`, `proc.spawn_wait.*` and `proc.posix_spawn_wait.*`,
  main before #1259
  against main after, four boots a side. Main after must be no slower than
  before; a spawn no longer copies a `/system` program's text, and whether
  that makes it faster is not measured on metal.

Until step 2's revocation exists, an object a space maps must keep its frames
for the object's life: ROOT's pages stay non-reclaimable, and ROOT is not
served through a pager, until then.

**Paged objects** (memory design §7, `issues/toyos-builds-itself.md` item 13)
are split in two. **7a**, delivered by the storage track's file-server work:
a pager's objects, read through `SYS_PAGED_READ` and never mapped
(`kernel/src/object/paged.rs`). **7b** stays this track's: mappings of them,
faults that park, their page tables and reverse records, holders' kernel
writes and their admission. **7b comes before stage 4**, by the
orchestrator's ruling of 2026-11-01 on the owner's delegation: file mappings
are on the critical path for file parity with Linux and for the storage
track's name cache, and stage 4's seal layers onto mappings rather than the
reverse. By the orchestrator's ruling of 2026-11-03, 7b needs two things:
the swap track's S0-a half that is met, every user copy outside every spin
lock (a fault that parks on a pager can hold none, and `user_ptr` refuses a
copy made under one); and, after the storage consolidation (#1152) lands, the
same gate extended to sleeping locks, so a user copy under any kernel lock is
refused. #1152 deleted the kernel's sleeping lock (`kernel/src/sleeplock.rs`),
so the spin-lock gate is that gate and both are met. The rest of S0-a's row
(a chunk faulted in before any lock and pinned after a re-check, the lost-page
outcomes, its H model) is built as part of 7b. One outcome 7b must place: a pipe read or message receive of at most
`frame_bytes::BOUNCE` bytes lets its bytes go, and a receive installs its
handles, before copying out to the caller. A lost page met in that copy must
not lose them. Faulting the destination in before the take is one way.
