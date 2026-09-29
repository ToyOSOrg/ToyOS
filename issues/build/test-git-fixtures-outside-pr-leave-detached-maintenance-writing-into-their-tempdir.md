---
status: open
kind: tooling
opened: 2026-09-29
---

# Test git fixtures outside `pr` leave git's detached maintenance writing into their TempDir

git 2.54 (the local `/usr/bin/git`; v2.54.0 source) starts `git maintenance run
--auto --detach` after a `commit`, `merge`, `fetch` or push. The process
outlives the command. It runs a geometric repack whenever `objects/17/` holds
two or more loose objects (`too_many_loose_objects` counts that shard × 256
against 256), and the repack writes into `objects/` while a `TempDir` drop runs
`remove_dir_all` on the repository. The result is `Directory not empty (os
error 66)`. `src/pr.rs`'s `repo` fixture now sets `maintenance.auto false` on
every repository it makes. The other fixtures set only identity and signing:
the `git` helpers in `src/compiler.rs`, `src/sysroot.rs`, `src/release.rs` and
`src/buildlock.rs`, and the `sh` config in `src/forkcheck.rs`.

Evidence: one `cargo test -p toyos-build --lib -- --test-threads=16` run with
`GIT_TRACE2_EVENT` set, after the `pr` fix, started 62 detached auto
maintenances. They came from the `compiler-*`, `fork-pins`, `fork-remove`,
`sweep`, `forkcheck-*`, `release-tag` and `buildlock-worktrees` fixtures, and
none came from `pr-*`. `issues/build/a-compiler-fixtures-git-commit-could-not-create-a-temporary-file.md`
is a failure inside a compiler fixture's object store, and nobody has measured
whether this is its cause.

**Exit**: that traced run starts no `maintenance run --auto` in any fixture.
