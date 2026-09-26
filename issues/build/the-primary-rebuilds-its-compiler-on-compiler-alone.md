---
status: assigned
kind: tooling
opened: 2026-09-26
---

# The primary rebuilds its compiler on `compiler/` alone

`src/toolchain.rs` rebuilds the primary's toolchain when the stamp over
`rust/compiler/` changes, and `compiler::record` writes that tree as the
compiler the primary's `stage2` is. A fork commit that moves only
`src/bootstrap`, `src/tools`, `src/stage0`, `Cargo.lock` or the LLVM submodule
leaves the primary on the compiler it had, and a worktree whose `compiler/`
matches the record is handed that compiler too. A worktree's own compiler is
keyed on all of them (`compiler::key`, PR #524), so the two answers to "which
compiler do these sources name" differ.

**Owner**: the ARM64 track (`issues/kernel/toyos-runs-on-arm64.md`), whose
PR #524 made the worktree key; it is closed before that track's stage 4
lands.

**Exit condition**: the primary's rebuild and its record read the same sources
`compiler::key` does, and a worktree compares against that, shown by a test
in which a fork moving only `src/tools` gets a compiler of its own.
