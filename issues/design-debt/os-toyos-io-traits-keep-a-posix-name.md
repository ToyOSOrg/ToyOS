---
status: owner
kind: question
opened: 2026-08-24
---

# Does `std::os::toyos::io` get to keep `AsRawFd`/`FromRawFd`?

The owner ruled on 2026-08-19 that **"fds belong only in libc jargon"**: the
kernel has no file descriptors, a process holds typed handles
(`toyos-abi/src/handle.rs`), and `userland/libc` is the one layer whose
interface the word describes. The sweep that followed took the kernel, the ABI,
the SDK, every forced call site and both fork pins. One name it did not settle
is left, and it is the owner's because it is a fork-repo name (`Japabu/rust`)
and not this tree's to rename alone.

`std::os::toyos::io::{AsRawFd, FromRawFd}`. The naming table ruled that
`os::toyos::*` is ToyOS's own extension API and must speak ToyOS, and it named
every `process` row — but not this one. std's *POSIX* surface keeps the word by
charter; whether an `os::toyos` trait carrying a POSIX name is that surface or
the extension API is what nobody has decided.

The two answers, both defensible:

- **It stays**, as a deliberate mirror of std's own
  `std::os::unix::io::{AsRawFd, FromRawFd}` convention — the trait names an
  `os::toyos` caller reaches for by muscle memory, and diverging costs every
  such caller a lookup.
- **It is renamed the next time the trait is touched in the fork**, to whatever
  `os::toyos` decides its own non-POSIX vocabulary for a raw handle is, because
  the extension API speaking POSIX is precisely what the ruling forbade
  everywhere else.

`tests/toyos-rust-tests/src/bin/std_fs.rs:4` is the only caller in this
repository (`use std::os::toyos::io::{AsRawFd, FromRawFd};`, used at `:59`).
Nothing is blocked on the answer; the word is legal in the two places it is
still spoken here, so this is a naming decision and not a defect.
