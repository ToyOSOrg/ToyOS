---
status: open
kind: defect
opened: 2026-09-30
---

# No guest case reads `std::filesystem`

libc++ for ToyOS is built with `std::filesystem`, and `<fstream>` with it,
over what libc gives `src/filesystem`: `setbuf`, `fseeko`, `ftello`,
`truncate`, `pathconf`'s `_PC_PATH_MAX`, and `utimes`, which refuses
`ENOSYS`, so `last_write_time` sets no time. `open` refuses every directory,
so no descriptor names one: `remove_all` walks a directory iterator
(`src/libcxx.rs`), which a link swapped in under it mid-walk can redirect, as
on libc++'s Windows. No C++ program in a guest uses any of it.

**Exit**: a guest test lists a directory through `std::filesystem`, and reads
a file back through `std::ifstream`.
