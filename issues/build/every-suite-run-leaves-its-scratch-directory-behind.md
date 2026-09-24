---
status: open
kind: tooling
opened: 2026-09-24
---

# Every suite run leaves its scratch directory behind, and the host fills up

`lane::dir()` is `$TMPDIR/toyos-tests-{pid}[/lane-N]` and nothing in the tree
removes one when its suite ends. The dev host collects them until guests start
failing with `No space left on device`.

Measured on the dev host on 2026-09-24: 1237 `toyos-tests-*` directories whose
process was gone held 84 GB, one to three GB each, and the volume was at 100%
with 4.2 GB free. A fast-tier `cargo test` running then reported nine reds on
`Failed to write test boot image: ... StorageFull`, none of them about its diff.

What a fix has to keep: a failing test's kept console (`<lane>/<test>/console.log`)
is named in its red and read after the run, so a directory holding a red is
evidence rather than garbage.
