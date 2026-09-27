---
status: open
kind: tooling
opened: 2026-09-27
---

# A toolchain release takes GitHub's Latest badge from the image release

`gh release create` in `src/release.rs` names no `--latest`, so GitHub marks
each new toolchain release Latest: every release this repository has published
is a toolchain's, and the newest carries the badge. The image release
(`src/imagerelease.rs`) is created `--latest`, and the next toolchain release
takes the badge back, so `/releases/latest` names a toolchain rather than the
image a person downloads until the next image release.

Not changed beside the image release because `src/release.rs` is one of the
trees the toolchain's tag hashes (`TREES`): editing it publishes a toolchain.

**Exit condition.** A toolchain release is created `--latest=false`, and
`/releases/latest` names the newest image release whenever there is one.
