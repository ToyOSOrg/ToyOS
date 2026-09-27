---
status: open
kind: defect
opened: 2026-09-27
---

# A poisoned thread's TLS is freed still mapped when its joiner collects it

A thread that dies in panic recovery is taken out of its process by
`poison::zombify_poisoned` from the idle loop, which frees nothing: its
`ThreadData`, and the `MappedPages` of its TLS block in it, stay in its
`ThreadEntry`. When the process is not being torn down, a sibling's
`SYS_THREAD_JOIN` collects that zombie (`join::collect_zombie`) and drops the
entry, and `MappedPages` dropped without its unmap returns the frames to the
PMM while the process's page tables still map them and its other threads still
run. A thread's own `SYS_THREAD_EXIT` unmaps first (`process::release_thread`);
the poisoned one never runs again to do so.

Reached only through a kernel panic recovered inside a multi-threaded process's
syscall. `toyos-proclife`'s model reports it as law L8 for any schedule that
collects a mapped thread with a sibling still in the process; no test scripts a
poisoned sibling and its joiner.

Exit condition: a poisoned thread's mappings are released before its entry can
be collected, or its entry is not collectable, with a model test that scripts
the poisoned sibling and its joiner.
