---
status: open
kind: defect
opened: 2026-09-01
---

# A `TcpStream` write to a departed peer is still `ErrorKind::Other`

Making a broken pipe reach a Rust caller as `BrokenPipe` put one exhaustive
`SyscallError -> ErrorKind` map in `rust/library/std/src/sys/pal/toyos/mod.rs`
and pointed `sys/pipe`, `sys/stdio` and `sys/fs` at it. Two maps in the same
fork were not moved and still carry a `_ => Other` arm, at fork commit
`719118153253`:

```rust
// library/std/src/sys/net/connection/toyos.rs:46
fn syscall_err(e: SyscallError) -> io::Error {
    match e {
        SyscallError::WouldBlock => io::ErrorKind::WouldBlock.into(),
        _ => io::Error::new(io::ErrorKind::Other, "syscall error"),
    }
}
```

```rust
// library/std/src/sys/process/toyos.rs:272
let kind = match e {
    toyos_abi::syscall::SyscallError::NotFound => io::ErrorKind::NotFound,
    _ => io::ErrorKind::Other,
};
```

The first is on a write path. `TcpStream::write`
(`library/std/src/sys/net/connection/toyos.rs:199`) ends in
`syscall::write(self.raw_handle(), buf).map_err(syscall_err)` at `:216`, and a
netd socket's tx end is a pipe — so a Rust program writing to a socket whose
peer has gone gets `ErrorKind::Other` — the same wrong word the pipe path was
just cured of, one layer down. `SyscallError::Gone` is the kernel's word for it
(`kernel/src/object/ops.rs:381`).

**And the claim that closed that one is only true of the three modules it
moved.** Its commit message and pull-request body said a new `SyscallError`
variant "now fails to compile until somebody decides its kind"; these two arms
would absorb one silently. The correction is recorded in this branch's history
and in the pull request, not left to be rediscovered here.

## What closing it takes

Pointing both at `sys::to_io_error` and deleting the local maps, as the three
already moved were. `sys/process/toyos.rs`'s is the spawn refusal and wants a
look at whether `NotFound` is doing work the shared map does not; the shared
map answers `NotFound` for the same variant, so it is likely a plain deletion.

The instrument is the differential the pipe arm already uses, one layer down: a
guest arm whose `TcpStream::write` to a peer that has exited must answer
`BrokenPipe`, against libc's `send` on the same shape. `netd_gone_mid_bind`
is the boot that already stages a departed netd, and its module header records
what today's `Other` costs `/system/bin/sshd`.
