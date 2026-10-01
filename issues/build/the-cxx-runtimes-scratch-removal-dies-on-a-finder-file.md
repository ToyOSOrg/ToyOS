---
status: open
kind: tooling
opened: 2026-10-01
---

# The C++ runtime's scratch removal dies on a Finder file

`libcxx::build` ends with a plain `fs::remove_dir_all(scratch)`. On this host
Finder writes a `.DS_Store` into a directory while it is being emptied. A
`cargo run -- --build-only` on `wt/toyos-rebuild` died with `remove
…/rust/build/sysroots/d9ce291c409918f4.libcxx-x86_64: Directory not empty (os
error 66)`, and afterwards the directory held only that `.DS_Store`. The
sysroot was left at `<key>.partial`, so the next build made it again.
`sysroot::remove_tree` is the removal that outlasts it.

**Exit**: every removal of a build product outlasts a writer that adds a
file while it runs, with a test that adds one.
