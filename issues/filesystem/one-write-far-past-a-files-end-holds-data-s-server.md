---
status: open
kind: defect
opened: 2026-09-27
---

# One write far past a file's end holds DATA's server for as long as the gap

fsd takes a `WRITE` at any offset up to `toyos::fs::MAX_FILE_BYTES` (16 TiB),
and `Mounted::resolve_or_alloc_block` bridges the gap from the file's end to it
by allocating and zeroing every block between
(`issues/filesystem/a-bcachefs-hole-costs-a-block-write-per-page.md` is why).
fsd is one thread, so the one request runs until the volume is full or the gap
is bridged, and every other client of `/apps`, `/config`, `/home` and `/state`
waits the whole of it: any program holding `/home` can stop every program's
files with one `seek` and one byte.

**Exit**: a write whose gap is past what a request may cost is refused by name
before anything is allocated — or the format expresses a hole — with a guest
test that writes one byte a gigabyte past a file's end and has another client
answered meanwhile.
