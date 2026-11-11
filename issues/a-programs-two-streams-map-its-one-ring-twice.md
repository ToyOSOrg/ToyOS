---
status: open
kind: defect
opened: 2026-09-26
---

# A program's two streams map its one ring twice

Found in the review of PR #527. The supervisor puts one log ring in a program's
slots 1 and 2 as two handles, and the stream state's `ask`
(`sdk/toyos/src/log/stdio.rs`) takes each stream on its own: it maps the region
its descriptor names, once for stdout and once for stderr. So every program
holds two 2 MiB mappings of the same ring. It holds them once per process,
whichever runtimes write, and duplicates no handle for them.

Nothing a program can ask today tells it that two handles name one object,
which is why `ask` does not share the mapping: assuming it would be wrong for
a child whose parent put two different rings there. A program whose two
descriptors share one table object, as the supervisor's own do, could be told
by the table, and that covers no program the supervisor starts.

**Exit condition**: one mapping per ring a process holds, shown by a guest
test that counts the process's mappings of its ring. Two ways there, and the
choice is the owner's since the first is a kernel answer: `fstat` of a memory
object answering an identity of the object (the kernel numbers every object
already and shows the number to nobody), which `ask` compares; or every
launcher handing the ring once with the second slot named its alias, as a
spawn already does for a file's shared description, which reaches `toyos::spawn`,
std's and libc's spawn and the launch protocol.
