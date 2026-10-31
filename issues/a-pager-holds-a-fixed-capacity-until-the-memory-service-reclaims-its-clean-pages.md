---
status: open
kind: track
opened: 2026-10-31
---

# A pager holds a fixed capacity until the memory service reclaims its clean pages

A pager's objects hold at most the capacity it was made with
(`kernel/src/object/paged.rs`, `toyos_abi::syscall::PAGER_MAX_PAGES`, 96 MiB).
Its core is promised at once: the dirty ceiling and up to 512 pages of
page-in headroom. Every other page is promised as it is supplied and given
back as it leaves. The DATA server asks for the whole 96 MiB, 32 MiB of it
dirty, which is the clean and dirty limits its block cache held before.

The capacity is the pager's own bound, so a machine with memory to spare
still evicts a file's pages at 96 MiB, and a machine short of memory cannot
take back a pager's clean pages; only the pager withdraws them
(`system/fileserver/src/pages.rs`, `Files::make_room`). The orchestrator ruled
this interim, with the rest of the charging (2026-10-29): the core is
resident by kind (`issues/anonymous-memory-swaps-to-an-encrypted-partition.md`)
and clean pages above it are reclaimable slack.

**Exit**: clean pages above a pager's core are slack the memory service
reclaims without asking the pager (memory design §7.4 rule 10), at P2 or
memory stage 8, and the fixed capacity goes.

**Owner**: the process-memory track
(`issues/process-memory-in-4-kib-pages-still-copies-each-programs-text.md`),
with the file server's sizing (`system/fileserver/src/main.rs`, `pager`).
