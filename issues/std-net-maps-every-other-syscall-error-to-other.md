---
status: open
kind: defect
opened: 2026-09-01
---

# std's net backend still maps every other `SyscallError` to `ErrorKind::Other`

Making a broken pipe reach a Rust caller as `BrokenPipe` put one exhaustive
`SyscallError -> ErrorKind` map in `sdk/std/sys/pal/mod.rs` and pointed
`sys/pipe`, `sys/stdio` and `sys/fs` at it. `sdk/std/sys/net/connection.rs`
keeps a map of its own with a `_ => Other` arm:

```rust
fn syscall_err(e: SyscallError) -> io::Error {
    match e {
        SyscallError::WouldBlock => io::ErrorKind::WouldBlock.into(),
        _ => io::Error::new(io::ErrorKind::Other, "syscall error"),
    }
}
```

A TCP stream no longer goes through it: `TcpStream` reads and writes through
`toyos::net::stream`, whose refusals `stream_refused` maps by name. What still
does is `UdpSocket`'s `recv_from` and `send_to`, which read and write the
socket's pipes to netstack, and the listener's notification reads. So a `UdpSocket::send_to` after netstack has gone
answers `ErrorKind::Other` where the pipe path answers `BrokenPipe`, by reading
the code, and a new `SyscallError` variant is absorbed there silently rather
than failing to compile until somebody decides its kind.

## What closing it takes

Pointing `syscall_err`'s callers at `sys::to_io_error` and deleting the local
map, as the three modules already moved were; and a test that a datagram
socket's write to a departed netstack answers `BrokenPipe`, against libc's
`sendto` on the same shape. `netstack_gone_mid_bind` is the boot that already
stages a departed netstack.
