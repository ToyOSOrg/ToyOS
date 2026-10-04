---
status: open
kind: defect
opened: 2026-10-04
---

# std names the SDK crates by path

`rust/library/std/Cargo.toml` takes `toyos-abi` from `../../../toyos-abi`
and `toyos` from `../../../toyos`, so the fork knows where this repository
keeps them. `.claude/agents/implementer.md` ("A fork") says a fork depends on
ToyOS crates by version, never by path; `vex-sdk`, three lines above in the
same file, is named by version.

It is stage 4 of `issues/the-tree-says-who-uses-each-thing.md`: while the
path stands, moving either crate breaks every fork commit pinned before the
move.

**Exit:** std names both by version, the build supplies them by a `[patch]`,
a clean toolchain build passes, and the sysroot key is unchanged by the
switch.
