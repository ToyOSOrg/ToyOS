---
status: open
kind: defect
opened: 2026-09-30
---

# libc has no alarm

libc neither declares nor defines `alarm`, and LLVM bounds its wait on a child
with one: a `SIGALRM` handler makes `wait4` answer `EINTR`
(`llvm/lib/Support/Unix/Program.inc`, `Wait`), so an LLVM built for ToyOS does
not link without it: rustc's LLVM wrapper linked `-shared -z defs` with the
libraries it needs leaves it undefined, beside stage 3's `wait`, `wait4` and
signal-set calls. POSIX gives `alarm` no refusal, and its `SIGALRM` reaches
a handler only through the signals libc imitates from stage 3 of
`issues/kernel/a-childs-end-is-an-event-and-a-parent-takes-its-children-down.md`
on; this waits on that stage.

LLVM's `Wait` disarms it with `alarm(0)` and then restores the old `SIGALRM`
action (`llvm/lib/Support/Unix/Program.inc`), so an `alarm(0)` that disarms
nothing ends the process when the alarm fires under the default action.

**Exit**: `alarm` arms and disarms `SIGALRM` as POSIX says, which a guest C
case shows: a handler installed without `SA_RESTART` runs, and a `wait4` on a
child that has not ended answers `EINTR`; and after `alarm(5)`, `alarm(0)`
answers from 1 to 5 and a second `alarm(0)` answers 0.
