---
status: open
kind: defect
opened: 2026-10-09
---

# A write over a file's committed page is not shadowed

DATA's commit (`bcachefs/src/fs.rs`, the read-write block's header) holds
every name, and each entry's length and extents, to one sync or the other,
and not a file's bytes. Fileserver's `write` (`userland/fileserver/src/data.rs`)
resolves a page the file's extents already reach to the block they name
(`Mounted::resolve_or_alloc_block`) and writes the page there, through the
cache, which may write the block out before the next sync. So a kill can leave
a file's committed blocks holding some pages of a write and the rest from
before it, under whichever length the committed entry names.

Read from the code, not measured: `bcachefs/tests/crash.rs` and fileserver's
kill tests overwrite no page a sync already committed.

## Exit condition

A write over a page a sync committed lands in a block no committed entry
names, and a host test that overwrites committed pages and stops the device at
every write of that write and its sync finds each file's bytes as one sync or
the other.
