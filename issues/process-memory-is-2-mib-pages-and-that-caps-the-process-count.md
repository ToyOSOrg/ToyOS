---
status: open
kind: track
opened: 2026-09-08
---

# Process memory is 2 MiB pages, and that caps the process count

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

**What this removes as a side effect:** a device window mapped at 4 KiB no
longer shares a 2 MiB page with a neighbour's registers, so the relocation
that `kernel/src/pcidev/` does before a hand-over is no longer needed for a
window firmware placed apart from its neighbours. Placing a window inside the
host bridge's firmware-reported apertures stays correct under either page size.

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
2. **Superframe frame allocator.** 4 KiB frames carved from 2 MiB
   superframes; the pin rule held per superframe, so no pinned frame is
   reissued and no superframe holding one is handed out whole; a watermark
   refuses an allocation before the machine runs dry. Exit: host tests of
   carve, free, pin and the refusal; every 2 MiB caller of the PMM unchanged;
   `user_copy_spans_windows` and `munmap_reissues_second_read_window` red
   again under their negative controls, because each is two physical runs
   only while the allocator hands out the lowest free frame first.
3. **4 KiB user leaves.** A software `OWNED` bit in the leaf is the ledger of
   the frames a process owns, with per-process counts; stacks are demand-zero
   and TLS is 4 KiB. A typed syscall value crossing a page is served in
   segments, so `is_user_object` stops refusing a straddle and
   `abuse_page_straddle`'s refusals become delivery verdicts. A 2 MiB window
   of 4 KiB leaves from arbitrary frames is up to 512 runs, so the one-run
   bound `user_ptr::window_split` panics on becomes reachable from userland
   and goes with the stage. Exit: a
   process's floor on the T14 against the 10 to 14 MB above.
4. **Demand-zero anonymous `mmap`.** A fault installs a 2 MiB leaf only for a
   whole aligned 2 MiB span of the mapping; unmapping a range no thread touched
   sends no IPI. `munmap` refuses a size that is not the whole mapping, by the
   orchestrator's ruling, and that is in: `SYS_MUNMAP` and a `FIXED` `mmap`
   over a mapping take only the length the mapping's `mmap` was asked for,
   compared as given, so this stage's rounding moves nothing either accepts.
   What the refusal costs a C program is
   `issues/libc-munmap-refuses-part-of-a-mapping.md`. Exit:
   `mmap` of more than the watermark allows refused at `mmap`, and a guest
   that touches one page of a large mapping holds one frame.
5. **File-backed faults at 4 KiB.** ROOT's read-only text is shared
   zero-copy. Exit: two processes of one binary hold its text once.
6. **`/apps` image sharing.** The design is the owner's decision, owed when
   this stage is reached.
7. **Retire 2 MiB where it no longer pays.** Exit: every remaining 2 MiB user
   leaf is a stage-4 whole span or a device or DMA grant.
