---
status: open
kind: defect
opened: 2026-09-29
---

# std reads a `SYS_QUERY_MODULES` byte count as a module count

The std fork's unwinder (`rust/library/std/src/sys/pal/toyos/mod.rs`,
`eh_frame::load_modules`) takes `query_modules`'s `Ok(n)` as a record count
and reads `size_of::<ModuleInfo>()`-byte records from offset 0 until one would
pass the end of its 4096-byte buffer. `n` counts **bytes**
(`toyos_abi::syscall::query_modules`'s doc), so every "record" after the real
ones is the packed path strings and then the zeroed tail of the buffer, read
as `base`/`text_end`/`eh_frame_hdr`. It also takes `n > buf.len()`, which means
nothing was written, as an answer and reads the zeroes.

No wrong frame has been seen: a path's ASCII read as a `u64` lies far above the
user half, so no user PC falls in the range it makes, and a zeroed record's
range is empty. That is luck in the bytes, not a check.

**Exit**: `load_modules` decodes the answer with `toyos_abi::syscall::modules`
over the `n` bytes the call reported, and grows the buffer to `n` when `n`
exceeds it.
