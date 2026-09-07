---
status: open
kind: defect
opened: 2026-09-07
---

# The word a metal verdict turns on is spelled twice, and neither speller reads the other

`kernel/src/arch/syscall/machine.rs:86` writes `quiesce("Rebooting.")` and
`src/bootlog.rs:15` declares `REBOOTING: &str = "Rebooting."` as the last line a
passing boot must leave. Two literals, no shared declaration: reword the
kernel's and every metal run refuses with `the log's last line is … and not
"Rebooting."` while the kernel is behaving exactly as intended.

It fails closed, so it is a maintenance hazard and not a hole — the same shape
as `issues/kernel/a-known-mask-is-copied-out-of-toyos-abi-by-hand.md`, and with
the same cause: the only crate a `no_std` kernel and a host build crate both
read is `toyos-abi`, and a change that adds a word there lands alone by the
abi-split rule.

**Exit condition**: the word declared once in `toyos-abi` and named by both
`quiesce` and `src/bootlog.rs`, so `git grep '"Rebooting\.'` returns the one
declaration. It is an ABI change and lands on its own single-commit branch.
