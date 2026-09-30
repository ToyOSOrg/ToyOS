---
status: open
kind: tooling
opened: 2026-09-30
---

# Stub worktrees build their toolchains one at a time

A linked worktree whose `rust/` is the stub builds every toolchain its key
lacks in the host's one shared checkout, `<primary>/rust/build/fork/`, held
exclusively for the whole build (`sysroot::Fork::checkout`). A second stub
worktree needing a key of its own waits behind the first: behind a compiler
build, which took 22:05 there (`Build completed successfully in 0:22:05`), or a
sysroot's std build, which took 6:29 (`0:06:29`), both from one
`cargo run -- --build-only` of this store's branch on this host. The old
layout built std in each worktree's own `rust/`, side by side. What the queue
costs with several stub worktrees building at once has not been measured.

Exit: the wait of several stub worktrees whose keys differ is measured on the
host and the owner accepts that number, or their builds no longer share one
checkout.
