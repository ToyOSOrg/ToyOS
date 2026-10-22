---
status: open
kind: finding
opened: 2026-10-21
---

# A comment says a second launch is refused where the supervisor panics

In `start` (`system/supervisor/src/main.rs`), the comment above the panic on a
`serves` row whose acceptor is spent says a second start "is refused by name
rather than spawned with a hole where its own service should be". The code
panics. No client can reach that panic: a launch reaches only a row its
caller's `starts` lists (`toyos_manifest::launch::may_start`), and the build
refuses a `starts` entry naming a row that serves a port or a role
(`starts_name_what_a_launch_can_start`, `build/build.rs`). So it is an internal
invariant, and the comment should say so.

**Exit**: the comment states the invariant and why no launch reaches it.
Whoever next edits that file folds this in and deletes the file.
