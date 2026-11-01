---
status: open
kind: defect
opened: 2026-11-01
---

# A test build waited forty minutes on its own process's sysroot lock

One `cargo test --test toyos-build` run, twelve wide, of eight guest tests
at `929e73d6c` (#1077's branch, on main `190f3b164`), on a macOS host, with
the sysroot rebuilt for an SDK change:

- 17:35:32: seven guest-binary builds wait for sysroot `f2cc0cfbcb345f2d`'s
  lock, "held by pid 86112 (building sysroot …)", which is the test
  process itself.
- 17:39:59 to 17:44: the sysroot is built, and six of the seven waiters go
  on by 17:45:07.
- 17:45:06: another worker's guest build (`shm_commit`) fails to compile:
  `cargo build failed`, the panic at `build/build.rs:494`.
- The seventh waiter, `stream_handles` for `stream_holders`, goes on
  waiting, "the holder left no readable note", until the run is killed at
  2611 s. Nothing compiles in the meantime, and the process has no
  children.

The next run, with the compile error fixed, did not wait. Whether the
failed build is the cause is not established; it is the only event of
that run that the others did not have. The log, with its paths masked, is
[`r3/logs/qemu1.log`](https://github.com/ToyOSOrg/evidence/blob/9ad74c276117e0418c04b546764e4afbada6f166/r3/logs/qemu1.log).

**Exit**: the wait reproduced and its cause fixed in `build/buildlock.rs`
or its callers, or the lock's acquisition shown unable to wait on a holder
in its own process.

**Owner**: the orchestrator, who assigns the build track.
