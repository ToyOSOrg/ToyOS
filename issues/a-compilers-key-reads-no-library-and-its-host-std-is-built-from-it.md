---
status: open
kind: defect
opened: 2026-10-08
---

# A compiler's key reads no `library/`, and its host std is built from it

A compiler is bootstrap's stage 2 of `compiler/rustc` and `library`
(`src/compiler.rs`, `build_in_fork`), so the `stage2` a store keeps carries a
std for the host, which every guest crate's build scripts and proc macros
link. Its key reads `compiler`, `src/tools`, `src/stage0` and `Cargo.lock`
(`KEYED`), and nothing of `library/`. A fork commit that moves only
`library/` keeps the compiler the store has, whose host std is the commit's
before. `src/keystore.rs` calls an input a build reads and its key does not a
defect of the key. True of a worktree's compiler since #524, and of every
compiler since the primary's became keyed.

The plain fix is `library` in `KEYED`. Its cost is a compiler build for every
fork commit that moves std, where such a commit builds only the freestanding
libraries and a sysroot today: by the phases of one build on an idle host,
5:19 for the compiler on top of 2:14 and about 1:30, so about 9 minutes
against 3:45, and about twice that under load. An estimate from those phase
times; nothing measured a `library/`-only commit. A runner pays its compiler
layer cold instead of restoring it. The other fix is a host std that is a
product of its own, cloned into a compiler as the freestanding libraries are
cloned into a sysroot.

Two refusals of the shape `compiler::place`'s has are untested: `sysroot.rs`'s
`build` and `build_freestanding` each refuse a product whose sources moved
while it was being built, and deleting either `assert!` passes every test.

**Owner**: the orchestrator, whose call the rebuild's cost is.

**Exit**: a test in which a fork commit moving only `library/` names another
compiler, or a compiler that carries no library its key does not read; and a
test each that reds when `build`'s or `build_freestanding`'s refusal is
deleted.
