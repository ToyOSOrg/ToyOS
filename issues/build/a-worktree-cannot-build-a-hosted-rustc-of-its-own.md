---
status: open
kind: tooling
opened: 2026-09-30
---

# A worktree cannot build a hosted rustc of its own

The ToyOS-hosted rustc is built by the primary checkout alone
(`src/toolchain.rs`'s `ensure`, under `Owner::Us`), from the primary's `rust/`
under the primary's `write_config`. A worktree's image with `hosted-rustc =
true` carries that one: refused by name when the worktree builds a compiler of
its own (`src/build.rs`, `env.primary_compiler`), and taken without a word when
only the worktree's `write_config` differs, so the image then carries a rustc
another tree's recipe built.

So no branch can put the hosted rustc it changes into a guest before it lands.
Carrying LLVM instead of Cranelift changes `write_config`'s hosted target and
the fork's `compiler/rustc_llvm` and `src/bootstrap`, and its guest test could
run only after the merge.

**Exit**: a worktree whose `write_config`, or whose fork's `compiler/` or
`src/bootstrap`, differs from the primary's builds a hosted rustc of its own,
keyed as `src/compiler.rs` keys a compiler, and its image carries that one.
