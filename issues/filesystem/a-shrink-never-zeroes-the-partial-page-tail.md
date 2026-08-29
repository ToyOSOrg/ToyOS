---
status: open
kind: defect
opened: 2026-08-28
---

# `set_size_locked` keeps the page a shrink lands inside and never zeroes its tail, so a regrow serves the discarded bytes instead of a hole

`ftruncate` to a size that is not a multiple of `PAGE_SIZE` drops every page
*past* the new end and leaves the page the new end falls **inside** exactly as
it was. Nothing zeroes the bytes between the new EOF and the end of that page,
in the cache or on the device, and nothing remembers that they are no longer the
file's. A later extend puts them back inside the file's valid range and the read
path hands them out.

`kernel/src/file_cache.rs:389-408` is the whole of it. On a shrink
(`new_size < file.size`, :393) it computes
`first_removed = (new_size as usize).div_ceil(PAGE_SIZE)` (:395) and removes
`file.pages.range(first_removed..)` (:396-400). `div_ceil` rounds up, so for
`new_size = 100` the removal starts at page 1 and page 0 — holding [100, 4096)
of the old file — is kept untouched. On a grow the function takes the
`} else { 0 }` arm (:402-404) and does nothing but `file.size = new_size` (:405).
`resize`, the form a user truncate calls (:381-387), adds only
`dirty_meta = true`.

The read path has no memory of the shrink. `valid_bytes_in_page`
(`file_cache.rs:454-461`) derives validity purely from the *current* `file_size`,
so once the page's start is below the regrown size the whole page reads as valid;
`copy_page_region_to_buf` (`:463-475`) then copies the raw resident bytes for
`[0, valid)` verbatim and `fill_zero`s only past `valid`. `read_page` serves the
resident page (`:211-215`) before it ever looks at `backing` (`:217`), so a
correctly sized backing cannot correct it.

`ops::ftruncate` (`kernel/src/object/ops.rs:560-572`) calls `file_cache::resize`
(`:566`) and clamps `state.position` (`:567-569`); there is no page-content
zeroing at the object layer either. `SYS_FTRUNCATE` is behind `Rights::WRITE`
alone (`kernel/src/arch/syscall/dispatch.rs:320-322`).

**The retention is not probabilistic.** On /tmp the file is
`create_file(false)` with no backing (`kernel/src/tmpfs.rs:76`), so `is_cache()`
is false and `page_at_or_after` skips the whole file (`file_cache.rs:535-540`) —
a tmpfs page is never evicted at all. On /home, a page written and then truncated
before any flush is dirty, and `evict_one` refuses a dirty page
(`file_cache.rs:510-512`). There is no window to lose.

**It becomes durable.** `flush_taken` writes the whole 4096-byte page,
stale tail included — `copy_page_out` is a raw `*buf = *page.data`
(`file_cache.rs:328-334`) and `fs.write_page` takes the array
(`kernel/src/vfs.rs:367-372`). Then `fs.update_metadata(file_id, size, mtime)`
(`vfs.rs:375-376`) records the regrown size, and on /home
`BcacheFsAdapter::update_metadata` (`kernel/src/bcachefs_adapter.rs:252-259`)
hands the *untrimmed* extent list to `bcachefs::Mounted::update_metadata`
(`bcachefs/src/fs.rs:917-943`), which rewrites `size` and keeps the extents. A
shrink therefore frees and zeroes nothing on the volume, and once the recorded
size is large again a fresh open's `NvmeBacking` is sized to cover those blocks
(`bcachefs_adapter.rs:159-170`) and serves them. The bytes survive eviction,
close, and reboot. `NvmeBacking::read_page` does bound at its own captured size
and pre-zero (`kernel/src/file_backing.rs:74-96`, `valid` at `:92`), but that
size is captured at open and never re-derived on a truncate — `flush_taken`
re-derives a backing only `if !has_backing` (`vfs.rs:379-384`) — so within the
open that did the shrink even the miss path returns the old bytes.

**Impact.** The universal filesystem invariant that a read past a former EOF
into a hole returns zeros is violated, silently and with the file's own prior
contents. `ftruncate` is the one primitive a program has to destroy the tail of a
file, and it does not: a program that shortens a file to discard data leaves that
data readable and, after a regrow, durably committed past what the file claims.
Paths are ambient (`kernel/src/vfs.rs` and `kernel/src/object/ops.rs` carry no
ownership or mode check), so the reader need not be the writer.

**Reproduction.** In-memory, no device, deterministic:

1. `open("/tmp/f", CREATE|WRITE)`; `write` 4096 bytes of a nonzero pattern.
2. `ftruncate(fd, 100)`.
3. `ftruncate(fd, 4096)`.
4. `lseek(fd, 0)`; `read` 4096 → bytes 100..4096 are the pattern, not zeros.

Durable variant: the same on `/home/f` with an `fsync` after (1), (2) and (3),
then close, reopen and read — the tail comes back off the volume.
`write`ing at an offset inside the hole instead of step (3) is the same door:
`apply_write` (`file_cache.rs:296-315`) raises `file.size` with no zero-fill.

**Nothing gates it.** `tests/toyos-rust-tests/src/bin/fs_truncate_persist.rs` is
the only `ftruncate` test in the tree. Its sizes are all page-aligned —
`PAGE = 4096`, `GROWN = 3 * 1024 * 1024`, `SHRUNK = 2 * PAGE` (`:16-18`) — so
`first_removed` always lands on a boundary and no partial page is ever retained,
and its one hole assertion (`:43-54`) is a grow of a never-shrunk file, where the
page is simply absent and the backing zero-fills. It never regrows after its
shrink (`:56-64`).

**Fix direction.** Zero the tail where the size is set, so no other path has to
know: in `set_size_locked`, when `new_size` is not page-aligned and the page at
`new_size / PAGE_SIZE` is resident, zero `page.data[new_size % PAGE_SIZE ..]` and
mark it dirty, so a flush carries the zeros to the device. That closes the
resident case and, once flushed, the device case for the straddled page. The
device tail *past* that page is a second half: on /home, `update_metadata` must
trim the extent list to `size` the way `Fat32Adapter` already does
(`kernel/src/fat32_adapter.rs:717-740`, `FileBlocks::truncate_to` at `:324-336`),
or the read path must bound a backing by the file's current size rather than the
size that backing captured at open. Checks the change owes: a negative control
that reverts the zeroing whole and shows the new test failing, and an
independent oracle — the POSIX/`ftruncate(2)` statement that extending a file
reads as zeros — as a test at deliberately non-aligned sizes on /tmp and /home,
covering both regrow-by-`ftruncate` and write-into-the-hole, before and after a
close/reopen.
