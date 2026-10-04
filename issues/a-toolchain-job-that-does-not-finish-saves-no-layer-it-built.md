---
status: open
kind: tooling
opened: 2026-10-02
---

# A toolchain job that does not finish saves no layer it built

`toolchain.yml`'s `build` makes every layer its restores missed in one step,
`cargo run -- --ci bootstrap`, and its four saves follow that step. A job that
ends inside it has saved nothing, whatever it had made: cancelled by a push to
its pull request, which cancels `ci.yml`'s run in progress, or red in the step
after its LLVM was placed.

Run 36913380100's `toolchain / build` (job 110541308331) ran 2:08:24, built
all four layers and reded in that step's last check, so its saves were skipped.
The next run, 36934214557,
missed all four restores and built the LLVM again, 1:29:15 of its 2:09:01
`--ci bootstrap` (job 110610545360).

Owner: the toolchain job (`.github/workflows/toolchain.yml`,
`release::bootstrap`).

**Exit**: each layer is saved by the step after the one that made it, and a
toolchain job stopped after its LLVM was placed leaves an entry the next run
restores.
