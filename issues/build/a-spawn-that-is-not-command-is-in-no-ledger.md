---
status: open
kind: defect
opened: 2026-09-02
---

# `Command::new` is one of several ways to run a binary, and the only one anything reads

`src/sourcegate.rs`'s `every_binary_the_host_runs_is_declared` reads every
`Command::new` argument in host Rust against a declared table. `Command` is not
how a host binary has to be started.

Measured 2026-09-02, planted in `src/ci.rs` and green with `218 passed`:

```rust
unsafe { libc::system(c"/usr/bin/curl --version".as_ptr()) }
```

`libc` is a **direct** dependency of `toyos-build` — `Cargo.toml` names it for
`statvfs`, `getloadavg` and the libproc pair — so this compiles today with
nothing added. A whole shell command line runs on the host and no gate sees a
binary at all. `libc::execvp`, `libc::posix_spawn` and a `sh -c` string handed
to any of them are the same hole under other names.

**Why the scan cannot simply grow a row for each.** `libc::system` takes a
`*const c_char`, so the name is not in the call at all — `c"…"` here, a
`CString` built three lines up in the next case. That is the same wall the
non-literal `Command::new` arguments hit, and there the answer was to pin them
to a file and a count; there is no equivalent when the argument is a pointer
into a buffer.

**The narrow thing that is worth doing on its own** is a ban rather than a
ledger: `libc::system`, `libc::execvp`, `libc::execve`, `libc::posix_spawn` and
`libc::fork` have no legitimate caller in this repository today, and a
`src/sourcegate.rs` `Ban` with an empty `allowed` list says so in the shape that
file already uses. That still leaves a hand-rolled `syscall!` and every crate
that spawns for us.

**Exit condition** is the one in
`issues/build/the-one-line-alias-rule-does-not-reach-a-brace-group.md`: a scan
that resolves names the way the compiler does, over the whole set of spawn
entry points rather than one spelling of one of them.
