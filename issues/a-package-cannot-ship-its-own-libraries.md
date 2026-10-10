---
status: open
kind: defect
opened: 2026-09-27
---

# A package cannot ship its own libraries

A program on a file server is spawned from a memory image its parent read
(`SpawnArgs::image`), and nothing loads a `DT_NEEDED` library for it: the
kernel loads none at spawn, and an executable that names one is refused at its
own start (`toyos::relocate`) until the userland loader of
`issues/the-kernel-still-parses-what-userland-writes.md` loads them. A package
under `/apps/<name>/` whose binary needs a library it carries beside it fails
to start.

## Exit condition

A spawn from an image loads a `DT_NEEDED` library from the package's own
directory, shown by a guest test that installs a package carrying one and
launches it.
