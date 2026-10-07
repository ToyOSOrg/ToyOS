---
status: open
kind: defect
opened: 2026-10-07
---

# std's ToyOS backend keeps assembly outside an architecture's module

`sdk/std/sys/pal/mod.rs` holds both architectures' naked `_start`, each under
its own `cfg(target_arch)`, and `sdk/std/sys/pal/tls.rs` holds x86-64's naked
`__tls_get_addr` and its slow path the same way. `.claude/agents/reviewer.md`
("Fit") keeps assembly, a naked function, a `core::arch` path and
`target_arch` in an architecture's own module and its selector. The rule did
not reach these files in the `rust` fork; they came under it when the backend
moved into this tree, and the move changed none of their code, so that it
could be checked as a move.

**Exit:** `target_arch`, `naked` and `core::arch` appear under `sdk/std` only
in an architecture's own module and its selector.
