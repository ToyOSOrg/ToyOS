---
status: open
kind: defect
opened: 2026-09-27
---

# A package cannot ship its own libraries

A program's own start loads the `DT_NEEDED` libraries it names (libc's
`__toyos_load_needed`), but a name without a `/` is looked for in
`/system/lib` alone (`sdk/libc/src/dl.rs`, `LIBRARY_DIR`). A package under
`/apps/<name>/` whose binary needs a library it carries beside it fails to
start.

## Exit condition

A spawn from an image loads a `DT_NEEDED` library from the package's own
directory, shown by a guest test that installs a package carrying one and
launches it.
