---
status: open
kind: tooling
opened: 2026-09-13
---

# An unknown flag to `cargo run` boots QEMU instead of being refused by name

`src/main.rs` reads its arguments by asking whether each known flag is present
(`args.iter().any(|a| a == "--build-only")` and twenty-odd more). Nothing asks
the opposite question, so a word the build system does not know is not refused —
it is ignored, and the run falls through to the default path, which builds an
image and launches a QEMU window on the owner's desktop.

Measured. `cargo run -- --help` from a worktree:

```
     Running `target/debug/toyos-build --help`
...
root: adding 'bin/compositor' (5266056 bytes)
...
[kernel 11424.092 cpu4]   shared-mem   alloc=     8 free=     0 held=     8 (16MB) rate=0/s
```

`--help` is not a declared flag. The guest booted and ran for 11,424 s of guest
time before it was closed.

**This is the one mistake the rule against `cargo run` exists to prevent**, and
the rule is the only thing standing in front of it: `CLAUDE.md` tells every agent
to verify through `cargo test` and never to run `cargo run` without a
non-launching flag, precisely because "a mistyped flag launches QEMU on the
owner's desktop". A mistyped flag should not launch anything. The project's own
principle is that the unimplemented dies loudly, and every other unknown name in
this tree is refused by name — `--kernel-feature` refuses a feature
`kernel/Cargo.toml` does not declare, and `actuator::init` panics on a boot
parameter the kernel declares no actuator for.

The cost is not only a window: the run holds a build slot and a guest slot for
as long as it lives, so every other worktree's build and every harness lane
queues behind a boot nobody asked for.

Exit condition: an argument matching no declared flag is refused by name before
any lock is taken, the way `--kernel-feature`'s is. `--help` then has an obvious
answer to give, which is the other half of why this was reached for.
