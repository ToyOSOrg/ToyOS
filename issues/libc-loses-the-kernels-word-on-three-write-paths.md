---
status: open
kind: defect
opened: 2026-08-31
---

# `send`, `sendto` and `fwrite` throw the kernel's error away

`userland/libc/src/posix_io.rs`'s `write(2)` passes the kernel's
`SyscallError` to `set_errno`, so a write whose reader is gone is `EPIPE` and a
full non-blocking pipe is `EAGAIN`. Three sibling paths do not:

* `userland/libc/src/socket.rs:354` — `send`'s TCP arm:
  `Err(_) => { set_errno(EIO); -1 }`. Every refusal is `EIO`, including the
  dead peer that `write(2)` reports as `EPIPE`.
* `userland/libc/src/socket.rs:430-431` — `sendto`'s UDP arm, the same
  `if let Err(_) = ... { set_errno(EIO); return -1; }`.
* `userland/libc/src/stdio.rs:32` — `sys_write` answers `-1` and sets **no**
  `errno` at all, so a failed `fwrite`/`fputs` leaves whatever the last failing
  call left behind.

The tx pipe on both socket paths is netd's, and netd exiting is exactly the
`Gone` the kernel now answers, so `send` on a dead netd says "I/O error" where
POSIX says "broken pipe" and where this same libc's `write` already says it.

Found while landing the kernel's `Gone` for a broken pipe; not fixed there
because none of the three is the defect that change was closing.

## What closing it takes

`Err(e) => set_errno(e)` at all three sites, and `sys_write` returning through
`set_errno` rather than a bare `-1`. `EIO` stays the answer only where the
kernel actually said `SyscallError::Io`.

The instrument is a guest arm through libc rather than through the SDK: a C
program whose `send` into a socket whose peer has gone must set `errno` to
`EPIPE`, measured against the same program's `write(2)` on a plain pipe, which
already does.
