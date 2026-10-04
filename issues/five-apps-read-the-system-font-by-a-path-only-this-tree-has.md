---
status: open
kind: defect
opened: 2026-10-04
---

# Five apps read the system font by a path only this tree has

An app builds and runs on Linux, macOS and Windows from the same source as on
ToyOS (root `CLAUDE.md`), and stage 7 of
`issues/a-package-is-a-directory-under-apps-and-the-installer-is-a-program.md`
builds each one outside this repository against published SDK crates alone.
Five of the six apps cannot, because of how they find JetBrains Mono:

- `editor` (`src/main.rs:1444`), `files` (`:60`) and `paint` (`:173`) read
  `/system/share/fonts/JetBrainsMono-Regular-8x16.font` and `expect` it, so on
  a host they build and then panic at start; `files` reads
  `/system/share/icons/*.svg` the same way (`:63-64`).
- `calc` (`build.rs:32`) and `snake` (`build.rs:7`) read
  `../../assets/JetBrainsMono-Regular.ttf` in their build scripts, a path
  into this tree.

**The owner ruled the fix on 2026-10-04**, recorded verbatim in
`issues/the-tree-says-who-uses-each-thing.md`: an SDK call answers the
platform's monospace font, `/system/share/fonts` on ToyOS and the platform's
own on Linux, macOS and Windows.

**Exit:** no app's source or build script names `/system/share` or a path
outside its own directory, each of the five takes its font from that SDK
call, and each starts on a host.
