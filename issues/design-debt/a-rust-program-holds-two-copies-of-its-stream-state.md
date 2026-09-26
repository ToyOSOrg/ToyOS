---
status: open
kind: defect
opened: 2026-09-26
---

# A Rust program holds two copies of its stream state

`std` links the SDK as its own dependency (`rust/library/std/Cargo.toml`,
`toyos` with `rustc-dep-of-std`), and a program that names `toyos` itself links
a second instance of the same crate. `toyos/src/log/stdio.rs`'s statics — which
slot each stream holds, the mapping of the log ring, the partial line each
stream is assembling, the cached pid — exist once per instance.

What that costs, today:

- Two mappings of one 2 MiB log ring and two duplicated shared-memory handles
  in every Rust program that says a line through `toyos::say!` and prints
  through `std` as well: every daemon.
- A partial line is joined only within one instance. `print!("a")` followed by
  `toyos::log::stdio::write(Stream::Out, b"b\n")` is two records on two lines,
  and a process's exit ends only what `std`'s instance holds —
  `tests/toyos-rust-tests/src/bin/console_line_atomicity.rs` writes its
  unended line through `std` for exactly that reason.

Both records reach the one ring, so nothing is lost and no line is spliced; the
weakness is the doubled state and the unjoined piece.

**Exit condition**: one instance of the stream state per process — `std`
re-exporting the SDK instance it links and the SDK crate taking it rather than
building its own, or the state moving to a per-process page both instances
find — shown by a guest test that joins `print!` and an SDK write into one
line and counts one mapping of the ring.
