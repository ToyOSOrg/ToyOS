---
status: open
kind: finding
opened: 2026-09-04
---

`forks.toml`'s `pr` field for `cpal`, `winit` and `softbuffer` names each
fork's precondition as "sendable once toyos-abi/toyos/toyos-window are on
crates.io". They published 2026-09-04 (run 33832842328), so the precondition
is met for all three; the sibling-tier forks (target state: disappear) are
ready to open against upstream, unopened.
