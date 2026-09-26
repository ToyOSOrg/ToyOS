---
status: open
kind: tooling
opened: 2026-09-26
---

# Issue files cite source paths that no longer exist

An issue names the site it is about by path, and a path is the claim a reader
checks first. Moving a source file leaves every issue that cites it pointing at
nothing, and nothing notices.

Measured on PR #524's branch at `73a89365` (the port's stage 0 moved the
kernel's x86 code under `kernel/src/arch/x86_64/`, the syscall handlers out of
`arch/`, and `hw.rs`): every `kernel/`, `src/`, `userland/`, `tests/`,
`bootloader/` and `toyos-*/` path written in `issues/`, checked against that
tree and against `origin/main`:

- 46 citations in 35 files name a path that exists on `main` and not on the
  branch: the branch moved them. Among them `kernel/src/arch/syscall/*.rs`,
  `kernel/src/hw.rs`, `kernel/src/mm/paging.rs`, `kernel/src/arch/apic.rs`,
  `kernel/src/arch/idt/*.rs` and `kernel/src/drivers/watchdog.rs`.
- Already on `main`, before this branch: at least 14 citations in 11 files of
  a whole `kernel/src/` path that does not exist (`kernel/src/arch/syscall.rs`,
  `kernel/src/inbox.rs`, `kernel/src/log_file.rs`,
  `kernel/src/completion/mod.rs` among them). The wider count, 153 in 95 files,
  also holds crate-relative spellings (`sched/dump.rs`) that the check cannot
  tell from rot.

**Whether a gate belongs with it.** `src/CLAUDE.md` says documentation carries
no gates, and an issue is prose; a red gate over `issues/` contradicts that
rule, so it is the owner's to decide. What the tree already requires of a
deleted document (its citations go in the same merge) is the rule a move needs
too, and an on-demand check that lists every cited path that does not resolve
(offline, beside `--check-forks`) would let the mover do it. A gate is what
makes the mover's duty hold. The on-demand check only makes it cheap.

**Exit condition**: every whole-repository path written in `issues/` resolves
in the tree that holds it, and the owner has ruled whether a move that strands
one is refused by a gate or caught on demand.
