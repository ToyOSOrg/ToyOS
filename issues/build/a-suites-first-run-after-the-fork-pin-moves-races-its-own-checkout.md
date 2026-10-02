---
status: open
kind: tooling
opened: 2026-10-02
---

# A suite's first run after the fork pin moves races its own checkout

`sysroot::fork_checkout` (`src/sysroot.rs`) moves a linked worktree's `rust/`
to the commit the tree pins when the checkout is behind it, with
`git checkout --detach`. The guest suite boots its tests on twelve threads and
each reaches `fork_checkout` on its own, so the first run after a merge that
moved the gitlink runs that checkout in every thread at once.

## Measured

A worktree at `bf28c1e38` whose `rust/` stood at `c4c65e3e8` under a pin of
`95960d6c2`, after a merge of `origin/main`: `cargo test --test toyos-build`
exited 1 with 18 of 26 red in 177 s. Every red is one of two sentences. One is
`git ["checkout", "--detach", "-q", "95960d6c2…"] in …/rust failed`, under
git's `Unable to create '…/modules/rust/worktrees/rust1/index.lock': File
exists`. The other is `…/rust is at c4c65e3e8… with uncommitted work`, from a
thread that read the checkout's status while another thread was moving it. The
checkout ended at the pin, and the next run of the same command did not repeat
either.

## Owner

Whoever next changes `sysroot::fork_checkout`.

## What would close it

One mover: the checkout is moved under a lock every thread of the process
takes, or once before the suite starts its workers, and
`cargo test --test toyos-build` on a worktree whose checkout is behind its pin
moves it and boots its tests.
