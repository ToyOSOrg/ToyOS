---
status: open
kind: defect
opened: 2026-09-01
---

# std on ToyOS answers `Other` for a device error, where upstream says `Uncategorized`

`sdk/std/sys/pal/mod.rs`'s one `SyscallError -> ErrorKind` map, `to_io_error`,
sends `SyscallError::Io` to `io::ErrorKind::Other`. Every other platform in the
fork spells that `Uncategorized` (`sys/io/error/unix.rs`, `hermit.rs`,
`uefi.rs`, `motor.rs`), and `Other` is documented upstream as a *custom* error,
not as the one a platform reaches for when it has no better word.

It was left because userland leaned on the spelling: logd's flush policy read a
stick that cannot flush as `ErrorKind::Other`, and a guest gate required
`kind=Other` for a boot volume that refused every read. Both are gone (logd is
logkeeper, which reads no kind; `tests/common/volumes.rs` is deleted), and no
`ErrorKind::Other` under `system/` means "the device refused".

## Exit condition

`Io => ErrorKind::Uncategorized` in `to_io_error`.
