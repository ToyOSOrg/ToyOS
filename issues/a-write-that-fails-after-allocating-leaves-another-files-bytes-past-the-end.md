---
status: assigned
kind: defect
opened: 2026-10-31
---

# A write that fails after allocating leaves another file's bytes past the end

DATA's `write` (`system/fileserver/src/data.rs`) allocates every page past
the file's extents first (`Mounted::allocate`) and then writes the pages
through the cache. If a page's `cache.write`, or the read of a page written
in part, fails partway, the call returns the error with the pages it never
wrote still in the file's extents, past its length. Those blocks hold what
their previous file left there: the allocator hands back recently freed
blocks first.

A later extending `truncate`, or a write beyond them, brings them under the
file's length. `allocate` zeroes only the hole from the end of the extents,
which already include them, so a read of that range returns the previous
file's bytes. That is one file's contents shown to another file's reader.

Found in review of PR #1118, from the code. The ordering, allocate all and
then write, predates that change.

**Exit**: a write that fails after allocating gives back the blocks it
allocated, as a refused allocation already does (`keep_blocks` to the
extents' old length, then `free_extents`). A host test fails a write's page
after its allocation, extends the file and reads zeros in the extended range.

**Owner**: the disk-path task, in the PR that follows #1118.
