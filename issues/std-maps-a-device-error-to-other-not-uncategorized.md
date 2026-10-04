---
status: open
kind: defect
opened: 2026-09-01
---

# The std fork answers `Other` for a device error, where upstream says `Uncategorized`

`rust/library/std/src/sys/pal/toyos/mod.rs`'s one
`SyscallError -> ErrorKind` map sends `SyscallError::Io` to
`io::ErrorKind::Other`. Every other platform in the fork spells that
`Uncategorized` — `sys/io/error/unix.rs:189`, `hermit.rs:29`, `uefi.rs:53`,
`motor.rs:55` — and `Other` is documented upstream as a *custom* error, not as
the one a platform reaches for when it has no better word.

It is deliberate, and this is the record rather than a rediscovery. The word is
load-bearing in userland: `userland/logd/src/policy.rs:12` states "a stick that
cannot flush is `io::ErrorKind::Other` — `SyscallError::Io`", and
`a_device_that_cannot_flush_still_ends_the_volume` at `:187` asserts against
that spelling. Moving the std arm alone would leave that header false while
nothing failed, because `fate`'s non-`WouldBlock` arms are catch-alls
(`_ => Fate::GiveUp` at `:166`) and the *behaviour* is identical either way.
That is the whole reason it was left: a silent divergence between a module
header and the code it describes is worse than a non-idiomatic arm.

## Exit condition

One change that moves all four together: `Io => ErrorKind::Uncategorized` in
the fork's map, `userland/logd/src/policy.rs`'s header reworded to name the new
spelling, its test's constructed kind moved with it, and
`boot_volume_metadata_error` in `tests/common/volumes.rs`, which requires the
guest to print `kind=Other` for a boot volume that refused every read — a fourth
site outside `userland/` and spelled `kind=Other`, so the grep below misses it
twice over and it is named here instead. It is closed when `rg
'ErrorKind::Other'` over `userland/` returns nothing that means "the device
refused" and that gate requires the new word.

Owned by whoever next touches either side; neither half is worth a landing on
its own, and the fork's upstream-mergeability argument is the thing that will
eventually force it, since `Other` in a platform map is what an upstream
reviewer asks about first.
