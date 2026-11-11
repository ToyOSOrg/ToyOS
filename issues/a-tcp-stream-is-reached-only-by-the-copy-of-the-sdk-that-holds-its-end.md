---
status: open
kind: defect
opened: 2026-09-26
---

# A TCP stream is reached only by the copy of the SDK that holds its end

`std` links the SDK as its own dependency (`rust/library/std/Cargo.toml`,
`toyos` with `rustc-dep-of-std`), a program that names `toyos` itself links a
second instance of the same crate, and libc links a third (`toyos_c`).
`sdk/toyos/src/net/stream.rs`'s table of ends is one per instance. A stream's
descriptor carries its instance's release function (`fd::STREAM`), so each end
is let go by the instance that holds it. Every call that asks an instance
about another's stream is refused, in a Rust program that also uses libc's
sockets:

- libc's `recv`, `send`, `sendto`, `recvfrom`, `getsockopt` and `getpeername`
  on a std stream's descriptor answer `EBADF`. libc's `read` and `write` of it
  work, through the descriptor table's owner, std's instance.
- std's `TcpStream::from_raw_fd` of a libc stream's descriptor panics
  (`Misuse::NotThisProcess`), and `File::from_raw_fd` of it reads and writes
  nothing (`NotAFile`).
- std's spawn of a libc stream's descriptor is refused `NotSupported`.
- A log line to stderr is dropped where stderr is a libc stream's descriptor.

The descriptor table and the stdio stream state are already one per process:
the instance that owns the table defines them and every instance calls it
(`toyos_fd_*`, `toyos_stdio_*`).

- **Owed on metal:** the stream state's test, the shared-boot member
  `stream_state_is_one`, on the T14's `testcases` boot, with that boot's
  `testcases-watchdog`, at main after #1226. `main` must show it green: pieces
  of one line from std, libc and the SDK are one record, and a redirect
  through libc or through the SDK is seen by the other runtimes. No other
  `testcases` member may have regressed, and `testcases-watchdog` must be
  green. The member runs only on metal.

**Exit condition**: one table of stream ends per process, held by the
descriptor table's owner as the stream state is, shown by a guest test that
`recv`s through libc on a std stream.
