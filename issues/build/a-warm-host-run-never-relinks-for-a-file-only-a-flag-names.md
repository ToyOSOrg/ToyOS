---
status: open
kind: tooling
opened: 2026-10-01
---

# A warm host run never relinks for a file only a flag names

A file that reaches a build only through a flag, a linker script named by
`-Clink-arg=-T…` in a `.cargo/config.toml` or in `RUSTFLAGS`, is read by the
linker, and cargo compares the flag, never the file. When that file sits
outside the package that links with it and changes alone, a warm `host` run
(`src/cicache.rs`) keeps the old link where a cold run makes a new one.

No flag the host job passes names a file: the tracked `.cargo/config.toml`
files pass none, and its steps set no `RUSTFLAGS`.

Done when a warm read refuses a flag that names a tracked file, or dates whole
the package that links with it, with a test.
