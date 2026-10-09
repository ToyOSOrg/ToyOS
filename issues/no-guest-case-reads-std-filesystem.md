---
status: open
kind: defect
opened: 2026-09-30
---

# No guest case reads `std::filesystem`

libc++ for ToyOS is built with `std::filesystem`, and `<fstream>` with it,
over what libc gives `src/filesystem`: `setbuf`, `fseeko`, `ftello`,
`truncate`, `pathconf`'s `_PC_PATH_MAX`, and `utimes`, which refuses
`ENOSYS`, so `last_write_time` sets no time, and `remove_all` walks a
directory by its path (`issues/remove-all-follows-a-link-swapped-in-mid-walk.md`).
Each of libc's is read back by `tests/testcases/tinycc/207_libc_names.c`
and `206_libc_refusals.c`; no C++ program in a guest uses any of it.

Owner: `issues/toyos-builds-itself.md`, M2.

**Exit**: a guest test lists a directory through `std::filesystem`, and reads
a file back through `std::ifstream`.
