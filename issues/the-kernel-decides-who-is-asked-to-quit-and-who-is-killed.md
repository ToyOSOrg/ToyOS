---
status: open
kind: finding
opened: 2026-11-11
---

# The kernel decides who is asked to quit and who is killed

An open design question, the orchestrator's to resolve, and an inference:
raised by the review of #1208, of the inventory in
`issues/the-kernel-is-small-interrupts-post-and-threads-wait.md`.

`SYS_PROCESS_QUIT` walks the subtree in the kernel, puts the reason in the
notice of each process that listens and kills each that does not
(`kernel/proclife/src/quit.rs`, 216 lines; `quit_process` in
`kernel/src/process.rs`; `kernel/src/object/notice.rs`, 61). The design
reads both ways: quitting gracefully is the parent's job, and interrupt,
hang-up and terminate requests are native events a program receives. Under
the first a parent would ask its children over its own protocol and the
kernel would keep only the kill; under the second the kernel's walk is the
mechanism those events need.
`issues/a-childs-end-is-an-event-and-a-parent-takes-its-children-down.md`
built it as its stage 6.

**Exit**: the design says which reading holds; this file is deleted, or the
walk's removal becomes a stage of the track.
