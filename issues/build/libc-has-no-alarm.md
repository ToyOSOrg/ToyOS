---
status: open
kind: defect
opened: 2026-09-30
---

# libc has no alarm

libc neither declares nor defines `alarm`, and LLVM bounds its wait on a child
with one: a `SIGALRM` handler makes `wait4` answer `EINTR`
(`llvm/lib/Support/Unix/Program.inc`, `Wait`), so an LLVM built for ToyOS does
not link without it
(`issues/build/toyos-libc-lacks-the-posix-surface-llvm-compiles-against.md`
measures that link). POSIX gives `alarm` no refusal, and its `SIGALRM` reaches
a handler only through the signals libc imitates from stage 3 of
`issues/kernel/a-childs-end-is-an-event-and-a-parent-takes-its-children-down.md`
on; this waits on that stage.

**Exit**: `alarm` arms `SIGALRM` as POSIX says, which a guest C case shows: a
handler installed without `SA_RESTART` runs, and a `wait4` on a child that has
not ended answers `EINTR`.
