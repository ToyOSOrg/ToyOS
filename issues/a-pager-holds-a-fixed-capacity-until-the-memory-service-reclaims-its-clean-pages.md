---
status: open
kind: track
opened: 2026-10-31
---

# A pager holds a fixed capacity until the memory service reclaims its clean pages

A pager's objects hold at most the capacity it was made with
(`kernel/src/object/paged.rs`). Its core is committed at its making and kept
for its life: the dirty ceiling and up to 512 pages of page-in headroom. Its
other pages are promised one at a time as they are supplied, and given back
as they leave. The kernel bounds a capacity by what the pager's job is
charged for its index, never by a constant.

The file servers size theirs as their block caches were sized before
(#1099, `system/fileserver/src/main.rs`, `pager`): together an eighth of
physical memory, 16 MiB each for LOG and BOOT and the rest for DATA, of
which 32 MiB may be dirty. The eighth is the bound netstack and the
compositor take for want of a kernel memory limit
(`issues/no-physical-memory-fairness.md`); it is an interim, not a sizing.

A pager also serves at most the objects it was made for: DATA one per 16
pages of its capacity, between 1,024 and 65,536, LOG and BOOT 1,024 each. So
the DATA server holds at most that many files' objects machine-wide; past it,
a file nobody holds goes to make room, and an open is refused `NoSpace` only
while every one is held open.

No other job can take a pager's clean pages back. A pager withdraws them
only for its own sake (`system/fileserver/src/pages.rs`, `Files::make_room`),
so with every pager full an ordinary job can be refused memory while an eighth
of the machine holds pages that could be read again from the disk. The
orchestrator ruled this interim with the rest of the charging (2026-10-29):
the core is resident by kind
(`issues/anonymous-memory-swaps-to-an-encrypted-partition.md`), and clean
pages above it are slack to reclaim.

**Exit**: clean pages above a pager's core are slack the memory service
reclaims without asking the pager (memory design §7.4 rule 10), at P2 or
memory stage 8, and `PAGES_SHARE` is deleted.

**Owner**: the process-memory track
(`issues/process-memory-in-4-kib-pages-still-copies-each-programs-text.md`),
with the file server's sizing (`system/fileserver/src/main.rs`, `pager`).
