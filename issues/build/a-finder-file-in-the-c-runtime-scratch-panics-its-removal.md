---
status: open
kind: tooling
opened: 2026-10-01
---

# A Finder file in the C++ runtime's scratch panics its removal

`libcxx::build` ends with `fs::remove_dir_all(scratch)`, and the macOS Finder
wrote a `.DS_Store` into that scratch while the removal ran: `remove
<primary>/rust/build/sysroots/acd58e51940b13be.libcxx-x86_64: Directory not
empty (os error 66)` at `src/libcxx.rs:95`, after the runtime had installed. The
`cargo run -- --build-only` that hit it exited 101.

**Exit**: a host writer in the scratch does not fail a build whose runtime
installed, with a test.
