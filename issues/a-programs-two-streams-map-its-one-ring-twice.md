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

**Each redirect onto a ring maps it again.** The table's owner forgets a
stream's answer at every change of descriptor 1 or 2, whichever runtime asked
for it, and the next write maps the ring the descriptor then names. A mapping
is kept for the process's life, because a write that read the old answer may
still be in it. So a program that redirects a stream onto a ring repeatedly,
even onto the same one, grows by 2 MiB of address space each time.

**Chosen exit (2026-11-11, in the review of #1226): every launcher hands the ring once, with
the second slot named its alias**, as a spawn already does for a file's shared
description. That reaches `toyos::spawn`, std's and libc's spawn and the
launch protocol, and needs no kernel change. A kernel answer of an object's
identity from `fstat` would do it too, but a kernel addition must do
something userland cannot, and this does not need one. With the two slots one
table object, the stream state keys its mappings by table object, so a
redirect back onto a ring it already mapped reuses that mapping.

**Exit condition**: one mapping per ring object the table holds, shown by a
guest test that counts the process's mappings of its ring after both streams
are written and again after a redirect away and back.
