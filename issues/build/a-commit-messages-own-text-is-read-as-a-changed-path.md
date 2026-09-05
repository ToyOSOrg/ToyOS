---
status: open
kind: tooling
opened: 2026-09-05
---

# `abi_lands_alone` reads a commit message's own text as a changed path

`src/pr.rs`'s `branch_commits` runs one
`git log --name-only --format='%x01%h %s%n%x02%b'` and classifies each output
line: `\x01` starts a header, `\x02` a body line, and **anything else is taken
for a changed file**. But `%b` is multi-line and only its *first* line gets the
`\x02` prefix, so every later line of a commit message body falls through to

```rust
last.touches_sysroot |= crate::toolchain::SYSROOT_SOURCES
    .iter()
    .any(|tree| line.starts_with(tree));
```

A commit whose message merely *mentions* `toyos-abi/src`, `toyos/src` or
`userland/libc/src` at the start of a line is then recorded as touching the
shared sysroot, and `--pr` refuses the branch as one that mixes sysroot sources
with work that depends on them.

## Reproduction

Commit `6b4566d1` on `t14-reboot-path` changes exactly one file,
`issues/hardware/the-t14-boots-toyos-unattended.md`, and its message quotes two
citations that wrap to the start of a line. `git log --reverse --no-merges
--name-only --format='%x01%h %s%n%x02%b' origin/main..HEAD` emits them
unprefixed:

```
toyos-abi/src toyos/src userland toyos-acpi/src -g '*.rs' -g '*.toml'` answers
toyos-abi/src/handle.rs:112) its only bit, and `run reboot` spawns
```

With that commit alone the branch passed, because `abi_lands_alone` returns
early when either partition is empty. Adding a second, genuinely non-sysroot
commit made the false partition visible and blocked the push.

## What is wrong and what is not

The refusal is the false positive's only symptom; a commit that really does
touch the sysroot is still caught. The hazard is the other way: a body line
beginning with a sysroot path is indistinguishable from a path, so **no reading
of this output can be trusted**, and the `Abi-Inseparable:` scan reads the same
lines. Separating the two — `--name-only` into its own `git log`, or a `-z`
format whose body cannot be confused with a name — is the exit condition.

Not fixed here: this was found by a branch it blocked, and the fix belongs to
whoever owns `src/pr.rs`.
