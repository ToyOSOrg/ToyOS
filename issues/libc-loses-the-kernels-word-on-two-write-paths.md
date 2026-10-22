---
status: open
kind: defect
opened: 2026-08-31
---

# `sendto`, `fwrite` and `fread` throw the kernel's error away

`sdk/libc/src/posix_io.rs`'s `write(2)` passes the kernel's `SyscallError` to
`set_errno`, so a write whose reader is gone is `EPIPE` and a full non-blocking
pipe is `EAGAIN`, and `send`'s stream arm answers through `stream_errno`
(`sdk/libc/src/socket.rs`). Two sibling paths do not:

* `sdk/libc/src/socket.rs`, `Datagrams::send` for `SocketEntry`, under
  `sendto`'s UDP arm: the write into netstack's pipe is
  `.map_err(|_| EIO)`. Every refusal is `EIO`, including netstack gone, which
  the kernel answers `Gone` and `write(2)` reports as `EPIPE`.
* `sdk/libc/src/stdio.rs`, `sys_write` and `sys_read`: a refused write or read
  answers `-1` and sets **no** `errno`, so a failed `fwrite`, `fputs` or
  `fread` leaves whatever the last failing call left behind.

## What closing it takes

The datagram arm answers the kernel's `SyscallError` through `errcode::of`,
whose mapping the host already tests, so `EIO` is the answer only where the
kernel said `SyscallError::Io`. `sys_write` and `sys_read` already hold an
`Errno` (`write_fd` and `on_fd`, `sdk/libc/src/posix_io.rs`) and set it.
