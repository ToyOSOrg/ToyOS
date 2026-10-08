---
status: open
kind: tooling
opened: 2026-10-08
---

# Two refusals of a product whose sources moved have no test

`src/sysroot.rs`'s `build` and `build_freestanding` each read their key again
once the product is built and refuse to publish it when the sources moved
meanwhile, as `compiler::place` and `llvm::place` do. Those two have a test
whose stand-in build moves a source; these two run bootstrap inline
(`build_std`), so no test can stand in for it, and deleting either `assert!`
passes every test. A sysroot or freestanding libraries built while
`toyos-abi` or the fork's `library/` was edited would then be served to every
checkout naming the key of the sources before the edit.

Owner: the step of `issues/the-forks-pin-is-a-file-and-a-worktree-checks-no-fork-out.md`
that rewrites the std build ("A pinned build reads an export"), which the
orchestrator briefs.

**Exit**: `build` and `build_freestanding` take the std build they run as
`compiler::choose` takes its compiler build, and a test each, whose stand-in
edits a keyed source before it returns, reds when the refusal is deleted.
