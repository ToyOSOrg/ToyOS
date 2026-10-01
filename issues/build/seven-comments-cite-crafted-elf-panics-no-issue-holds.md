---
status: open
kind: tooling
opened: 2026-10-01
---

# Seven comments cite crafted-ELF panics in `issues/`, and no issue holds them

`git grep -n 'crafted-ELF' HEAD -- '*.toml' src/` finds the citation at
`Cargo.toml:211,231`, `bootloader/Cargo.toml:45`, `kernel/Cargo.toml:376`,
`userland/Cargo.toml:69` and `src/build.rs:410,635`. No issue names either
panic: `git grep -i -e phnum -e 'gnu\.hash' -e bloom_shift 1e4d3e0ec -- issues/`
exits 1. The closed entry left the tracker at `fa2799ded`, and `da3f56573`
then rewrote the citations' path to `issues/`.

**Exit**: the clause is deleted at all seven sites, and the `git grep` above
finds no `issues/` citation.
