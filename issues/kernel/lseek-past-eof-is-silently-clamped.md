---
status: open
kind: defect
opened: 2026-08-28
---

# `ops::seek` clamps a seek past EOF to the file size, so an lseek past the end silently lands at EOF and a later write starts there rather than where the caller asked

`ops::seek` computes `position = new_pos.min(size)`
(`kernel/src/object/ops.rs:449`), so `lseek(fd, offset, SEEK_SET)` with
`offset > size` returns `size`, not `offset`. POSIX `lseek(2)` permits a seek
past the end: the position is set to exactly `offset`, and a later write there
extends the file, leaving a hole of zeros between the old end and the write — a
sparse file. ToyOS instead clamps the position to EOF and returns the clamped
value, so:

- **The returned offset is a wrong answer the caller trusts.** A program that
  seeks to `offset` and writes believes its bytes land at `offset`; they land at
  the old EOF, silently, and the return value does not say so.
- **A sparse file cannot be made through `lseek`+`write`.** The hole the caller
  intended is closed with no diagnostic.

**Reproduction.** Create a file, `write` 100 bytes, `lseek(fd, 200, SEEK_SET)` →
returns 100, not 200; `write` one byte → the byte lands at offset 100 and the
file is 101 bytes, where POSIX gives a 201-byte file with a 100-byte hole.

Found while writing the filesystem transactionality control
(`tests/toyos-rust-tests/src/bin/fs_transactional.rs`), which sidesteps it by
regrowing with `ftruncate` before writing into the hole. Orthogonal to the
rename/shrink transactionality defects.
