---
status: open
kind: defect
opened: 2026-09-26
---

# A Rust program holds two copies of its stream state

`std` links the SDK as its own dependency (`rust/library/std/Cargo.toml`,
`toyos` with `rustc-dep-of-std`), and a program that names `toyos` itself links
a second instance of the same crate. `sdk/toyos/src/log/stdio.rs`'s statics — which
slot each stream holds, the mapping of the log ring, the partial line each
stream is assembling, the cached pid — exist once per instance.

What that costs, today:

- Two mappings of one 2 MiB log ring and two duplicated shared-memory handles
  in every Rust program that says a line through `toyos::say!` and prints
  through `std` as well: every daemon.
- A partial line is joined only within one instance. `print!("a")` followed by
  `toyos::log::stdio::write(Stream::Out, b"b\n")` is two records on two lines,
  and a process's exit ends only what `std`'s instance holds.

Both records reach the one ring, so nothing is lost and no line is spliced; the
weakness is the doubled state and the unjoined piece.

The same holds of TCP streams. `sdk/toyos/src/net/stream.rs`'s table of ends
is one per instance, and so is libc's, which links an instance of its own
(`toyos_c`). A stream's descriptor carries its instance's release function
(`fd::STREAM`), so each end is let go by the instance that holds it. Every call
that asks an instance about another's stream is refused, in a Rust program
that also uses libc's sockets:
- libc's `recv`, `send`, `sendto`, `recvfrom`, `getsockopt` and `getpeername`
  on a std stream's descriptor answer `EBADF`. libc's `read` and `write` of it
  work, through the descriptor table's owner, std's instance.
- std's `TcpStream::from_raw_fd` of a libc stream's descriptor panics
  (`Misuse::NotThisProcess`), and `File::from_raw_fd` of it reads and writes
  nothing (`NotAFile`).
- std's spawn of a libc stream's descriptor is refused `NotSupported`.
- A log line to stderr is dropped where stderr is a libc stream's descriptor.

**Exit condition**: one instance of the stream state and of the table of
stream ends per process — `std`
re-exporting the SDK instance it links and the SDK crate taking it rather than
building its own, or the state moving to a per-process page both instances
find — shown by a guest test that joins `print!` and an SDK write into one
line and counts one mapping of the ring, and by one that `recv`s through libc
on a std stream.
