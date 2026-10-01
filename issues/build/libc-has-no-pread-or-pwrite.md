---
status: open
kind: defect
opened: 2026-09-30
---

# libc has no pread or pwrite

libc defines neither `pread` nor `pwrite`. The two it had sought the
descriptor to their offset, read or wrote, and sought back: three calls,
nothing held between them, so a `read`, `write` or `lseek` of the same
descriptor on another thread landing between them read or wrote at the wrong
offset, or had its own moved. The ABI has no positional read or write
(`toyos-abi/src/syscall.rs` has `seek`). With no header declaring `pread`,
LLVM's configure finds none, and LLVM reads a file slice with `lseek` and
`read` instead (`llvm/lib/Support/Unix/Path.inc`, `readNativeFileSlice`), which
moves an offset another thread shares just as they did.

**Exit**: `pread` and `pwrite` are declared and defined and never move the
descriptor's offset, which a guest C case shows over a file whose every 8-byte
word holds its own offset. While one thread `read`s the file through, another
`pread`s it at one offset, and every word either reads holds the offset it was
read from. While one thread `write`s such words through a second file, another
`pwrite`s one at its offset, and every word of that file then holds its own
offset. A seek, read or write, seek-back `pread` and `pwrite` are the negative
control: the case is measured red against them.
