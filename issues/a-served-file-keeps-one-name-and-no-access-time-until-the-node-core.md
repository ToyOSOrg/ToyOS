---
status: open
kind: defect
opened: 2026-10-23
---

# A served file keeps one name and no access time until the node core

File protocol v2 (`sdk/toyos/src/fs.rs`) carries hard links, `LINK_REPLACE`,
unnamed files, access and change times and a link count. Today's servers
are path-keyed (`system/fileserver/src/volume.rs`) and keep none of them, so
`system/fileserver/src/main.rs` answers:

- `LINK` and `CREATE_UNNAMED`: `Unsupported`, so libc's `link`, `linkat` and
  `O_TMPFILE` and std's `hard_link` fail `ENOTSUP`;
- an access time to set: `Unsupported`, so `utimensat` and `utimes` with a
  null `times`, which set both times, fail `ENOTSUP`; a modification time is
  set on DATA's files and refused on FAT's and on a directory;
- a link count of 1, no access or change time, and the server's pid as the
  volume, so `st_dev` changes at a restart;
- a held directory by its volume path, which the server moves with each
  rename it makes and marks removed at its removal.

The kernel's `/tmp` keeps no second name, unnamed file or set time
(`sdk/toyos/src/files.rs`, `NotSupported`), and a descriptor of one of its
directories names its absolute path, so a path from it is that path's,
lexically, and a rename of the directory is not followed.

`tests/corpus/tinycc/214_at_calls.c` asserts each refusal on `/home` and
`/tmp`.

**Exit**: the node core and its memory-format adapter (storage C2) serve
`LINK`, `LINK_REPLACE`, `CREATE_UNNAMED`, the access time and the format's
link count, and the bcachefs adapter (C3) does on a durable DATA with
`st_dev` derived from the volume's UUID; `214_at_calls`' refusals on `/home`
become their effects. `/tmp` served by the memory format (storage W3) ends
the kernel's half.
