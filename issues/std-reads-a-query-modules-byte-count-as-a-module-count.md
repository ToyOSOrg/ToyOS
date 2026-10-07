---
status: open
kind: defect
opened: 2026-09-29
---

# std reads a `SYS_QUERY_MODULES` byte count as a module count

Two readers, one in std's ToyOS backend and one in the fork's backtrace crate,
take `query_modules`'s `Ok(n)` as a record count
and read `size_of::<ModuleInfo>()`-byte records from offset 0 until one would
pass the end of their 4096-byte buffer:

- the unwinder, `sdk/std/sys/pal/mod.rs`,
  `eh_frame::load_modules`, reads each as `base`/`text_end`/`eh_frame_hdr`;
- the backtrace crate, `rust/library/backtrace/src/symbolize/gimli/libs_toyos.rs`,
  `native_libraries`, makes a `Library` of each whose one segment is
  `text_end - base` bytes long, a subtraction that can underflow on them.

`n` counts **bytes** (`toyos_abi::syscall::query_modules`'s doc), so every
"record" after the real ones is the packed path strings and then the zeroed
tail of the buffer. Both also take `n > buf.len()`, which means nothing was
written, as an answer and read the zeroes: the call answers a short buffer
with `Ok`, so neither ever reaches the `Err` arm that grows its buffer.

No wrong frame has been seen: a path's ASCII read as a `u64` lies far above the
user half, so no user PC falls in the range it makes, and a zeroed record's
range is empty. That is luck in the bytes, not a check.

**Exit**: `load_modules` and `native_libraries` each decode the answer with
`toyos_abi::syscall::modules` over the `n` bytes the call reported, and grow
the buffer to `n` when `n` exceeds it.
