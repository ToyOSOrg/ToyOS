---
status: open
kind: tooling
opened: 2026-10-01
---

# A warm host run trusts cargo for what a build reads outside its package

The merge queue's `host` run reads main's cache entry by content
(`src/cicache.rs`): every file of a package that changed is dated now, so
whatever a build reads inside its own package rebuilds it. An input outside
that package reaches a warm run only through what cargo is told, a build
script's `rerun-if-*` lines and rustc's dep-info. A path build script or proc
macro that reads another package's file, an environment variable or a tool
without declaring it passes stale there, where before every path crate
recompiled; only the cold nightly finds it, after main has moved.

None does in the host job today: its one path build script, `userland/calc`'s,
declares the font it reads from `assets/`, and no path crate is a proc macro.

Done when a test in `src/cicache.rs` whose path build script reads another
package's file without declaring it, read warm after a change to that file,
sees the build script run again.
