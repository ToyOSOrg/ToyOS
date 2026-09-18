---
status: open
kind: tooling
opened: 2026-09-13
---

# Two worktrees provisioning the toolchain's cargo leave it with none

`src/toolchain.rs:940` refuses a run whose toyos toolchain has no `cargo`:

```
the toyos toolchain at /Users/jan/Dev/jan/toyos/rust/build/aarch64-apple-darwin/stage2/bin
is missing cargo, so rustup answers for it by falling back to another toolchain
and narrating it on every invocation.
provision_toolchain_cargo is the step that puts them there, and it did not.
```

That refusal is right about the state and wrong about the cause: the step did
run, in another worktree, at the same time. The `bin` directory is the primary
checkout's and shared by every worktree, and the provisioning step takes the
build lock as `exclusive, give the toyos toolchain its own cargo` — but a
worktree that *already holds* the shared `test binaries` lock queues behind the
exclusive phase and then proceeds against a directory the exclusive holder was
midway through rewriting.

## Where it was seen

Twice in one session on a host running ten worktrees, both times inside
`cargo test <name>` and both times immediately after another worktree's
`rebuild the shared sysroot from a linked worktree`. The transcript either side
of the panic is the lock narration:

```
[build-lock] acquired (exclusive, give the toyos toolchain its own cargo) after 4.8s
[build-lock] waiting for the build lock (shared, test binaries) — an exclusive phase is queued ahead of it
[build-lock] acquired (shared, test binaries) after 77.7s
thread 'main' panicked at src/toolchain.rs:940:5
```

A plain re-run passed both times with nothing changed, which is what makes it a
race rather than a broken checkout — and what makes it expensive: on a
contended host a rebuild is four to eight minutes of queueing before the panic.

## What would show it

Two worktrees told to provision at once, with the second asserting the
directory it finds is either complete or refused for that reason rather than
for the state a concurrent writer left. `src/buildlock.rs` already names its
holders; what it does not do is make this step's readers wait on the writer.
