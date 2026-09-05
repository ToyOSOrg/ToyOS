---
status: open
kind: defect
opened: 2026-09-05
---

# `stats` does not resolve the command its user typed, so a relative one is refused

`userland/toybox/src/stats.rs:12` spawns what it was given, unchanged:

```
    let mut child = Command::new(&args[0])
        .args(&args[1..])
```

`args[0]` is a path its user typed, and `/system/bin/init`'s launcher refuses a
launch path that is not already canonical. So `stats ./foo`, `stats sub/foo` and
`stats ../bin/foo` answer

    stats: failed to spawn ./foo: ...

with `init: launcher: "./foo" is not a canonical path` in the log, while
`stats /system/bin/foo` and `stats foo` both work — the second because a bare
name goes through std's `PATH` walk, which hands init an absolute path.

It is the one program left that takes a path from a user and spawns it without
resolving: `/system/bin/shell` is the other, and does.

## The fix

`toyos_manifest::package::launch_path(&args[0], &cwd)`, the same call
`userland/shell/src/main.rs` makes, with `cwd` from `env::current_dir()` — a
launch carries a working directory and init applies it, so `stats` has the one
its caller gave it. `toybox` would take `toyos-manifest` as a dependency, which
`shell` and `init` already do.

## Reproduction

From a shell, in a directory holding a runnable `foo`: `stats ./foo`.

The refusal's wording is measured — `pkg_install_gbae`'s symlink probe reads
that line back for four spellings — and that `stats` passes `args[0]` through is
read from the source above; the two have not been put together in a run.

## Exit condition

`stats ./foo` runs `./foo`, and a guest probe spawns `stats` with a
`current_dir` and a relative command and reads the child's output back.
