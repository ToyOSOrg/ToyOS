---
status: open
kind: tooling
opened: 2026-10-02
---

# A worktree's first wide run reds on the fork checkout it is still making

`sysroot::fork_checkout` makes a new worktree's `rust/` with `git worktree add`
under no lock a second thread of the same process waits on: it tests
`rust/.git`, which exists from the first moment of the checkout, and every
other worker then takes the checkout as made while git is still writing its
files.

The first `cargo test --test toyos-build -- screen_` in a new worktree, three
wide, on a host at load average 35 of 14 cores: one worker printed `Making
…/rust a fork checkout`, and within 0.4 s the other two panicked in
`llvm::refuse_uncommitted_bootstrap` (`src/llvm.rs`) on `rust/src/bootstrap
holds changes no commit does`, listing every file of `src/bootstrap` as `D`.
`screen_panic_muted` and `screen_fatal_halt_composited` red with that
sentence, the run exited 1, and the same command passed once the checkout
existed.

**Exit**: the first guest run of a new worktree is green at any width.
