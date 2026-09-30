---
status: open
kind: track
opened: 2026-09-30
---

# Ninja runs every command through a `/bin/sh` ToyOS does not have

Ninja starts each build edge as `/bin/sh -c <command>` (v1.13.1
`src/subprocess-posix.cc:132-134`), and the command is the build file's own
text, written for a POSIX shell; tokio's `tests/process_smoke.rs` runs `sh -c`
too. ToyOS has no `/bin`: `/` holds exactly the names `kernel/src/vfs.rs`'s
`ROOT_ENTRIES` lists. Its shell is `/system/bin/shell`, which takes `-c`;
nothing establishes that it runs the POSIX shell language build files are
written in.

Blocked on `posix_spawn`, stage 3 of
`issues/kernel/a-childs-end-is-an-event-and-its-tree-is-a-job.md`. Where the
path comes from is
`issues/isolation/every-program-sees-only-the-files-it-was-given.md`'s.

**Exit**: inside ToyOS, Ninja builds a file whose commands chain with `&&`,
redirect and quote, through `/bin/sh`.
