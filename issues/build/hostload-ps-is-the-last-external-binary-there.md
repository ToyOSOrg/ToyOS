---
status: open
kind: defect
opened: 2026-08-26
---

# `tests/common/hostload.rs::process_names` still shells out to `ps`

Split out of `issues/build/df-ps-and-find-are-declared.md` (closed
2026-08-26) when its `df` and `find` call sites were replaced — `df -k` with
`libc::statvfs` in `src/worktree.rs`, `find -delete` with a recursive walk in
`toyos-fat32/tests/common/mod.rs` — leaving `ps -Ao comm=` as the one
remaining external binary that issue named.

`process_names` (`tests/common/hostload.rs`) wants every process's
executable basename, for gate A's `host:` line — a diagnostic, `.ok()?`
throughout, so its absence costs a line and not a run. The libc-crate
replacement is `sysctl(CTL_KERN, KERN_PROC, KERN_PROC_ALL, 0)`, but unlike
`statvfs` (a stable, POSIX-standard struct the `libc` crate exposes on every
target it supports), `libc`'s macOS bindings do not carry `kinfo_proc` or
`extern_proc` — checked directly against
`~/.cargo/registry/src/*/libc-0.2.174/src/unix/bsd/apple/mod.rs`, which
declares `KERN_PROC_ALL` and nothing shaped like the struct `sysctl` would
fill in. Reaching it correctly means hand-rolling a `#[repr(C)]` struct
matching Darwin's `kinfo_proc`/`extern_proc` layout from system headers
(`/usr/include/sys/sysctl.h`, `/usr/include/sys/proc.h`) rather than a typed
binding an upstream crate maintains — a correctness risk this batch could
not clear cleanly in the time available, unlike the other two call sites.
