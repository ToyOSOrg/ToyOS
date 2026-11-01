---
status: open
kind: defect
opened: 2026-10-31
---

# A zero-length write past the end extends a DATA file

DATA's `reserve` (`system/fileserver/src/data.rs`), which a write takes first, sets the file's length to at
least the write's end even when it writes no bytes. So a zero-length write
at an offset past the end makes the file that long, and stamps its time.
POSIX `write()`: "If nbyte is zero and the file is a regular file, the
write() function may detect and return errors as described below. In the
absence of errors, or if error detection is not performed, the write()
function shall return zero and have no other results." Nothing stale is
exposed: the pages past the extents read zeros.

Found in review of PR #1120, from the code.

**Exit**: a write of no bytes leaves the file's length and time as they
were, shown by a host test of a zero-length write past the end.

**Owner**: the file server (`system/fileserver/`).
