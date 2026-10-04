---
status: open
kind: defect
opened: 2026-09-27
---

# A package cannot ship its own libraries

A program on a file server is spawned from a memory image its parent read
(`SpawnArgs::image`), and `kernel/src/loader/mod.rs`'s `load_needed_libs` then
finds its `DT_NEEDED` libraries in `/system/lib` alone: the kernel opens no path
a file server holds. A package under `/apps/<name>/` whose binary needs a
library it carries beside it fails to spawn, where before the file servers
moved out of the kernel the executable's own directory was searched first.

## Exit condition

A spawn from an image loads a `DT_NEEDED` library from the package's own
directory, shown by a guest test that installs a package carrying one and
launches it.
