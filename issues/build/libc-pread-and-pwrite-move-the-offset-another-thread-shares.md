---
status: open
kind: defect
opened: 2026-09-30
---

# libc's pread and pwrite move the offset another thread shares

`pread` and `pwrite` (`userland/libc/src/posix_io.rs`) seek the descriptor to
their offset, read or write, and seek it back: three calls, nothing held
between them. POSIX has them leave the file offset alone. Here a `read`,
`write`, `lseek`, `pread` or `pwrite` of the same descriptor on another thread,
landing between those calls, reads or writes at the wrong offset, or has its
own moved. The ABI has no positional read or write (`toyos-abi/src/syscall.rs`
has `seek`). No header declares `pread`,
so LLVM's configure finds none; once it does, LLVM reads every file slice
through it (`llvm/lib/Support/Unix/Path.inc`, `readNativeFileSlice`).

**Exit**: `pread` and `pwrite` never move the descriptor's offset, which a
guest C case shows over a file whose every 8-byte word holds its own offset.
While one thread `read`s the file through, another `pread`s it at one offset,
and every word either reads holds the offset it was read from. While one
thread `write`s such words through a second file, another `pwrite`s one at its
offset, and every word of that file then holds its own offset. Today's `pread`
and `pwrite` are the negative control: the case is measured red against them.
