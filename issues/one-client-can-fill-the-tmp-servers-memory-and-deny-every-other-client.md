---
status: open
kind: defect
opened: 2026-11-12
severity: [security]
---

# One client can fill the `/tmp` server's memory and deny every other client

A memory volume's files are bounded only by what its server's job can be
charged (`pager` in `system/fileserver/src/main.rs`). Everything a `/tmp` file
costs is the MEMORY file server's:

- its object's slot in the kernel (`kernel/src/object/paged.rs`);
- the server's own record of it;
- its pages, up to the pager's capacity.

A directory or a link there costs the server's record alone. None of it is
charged to the client that made it, and nothing bounds one client's part. So
one client can make files, directories and links in `/tmp` until the memory is
spent. Then every other client's create there is refused. The server's next
allocation of its own may be among the refused, which ends it, and with it
every client's `/tmp`. The memory is counted to the file server, so nothing
names the client that took it (`issues/no-physical-memory-fairness.md`).

**What one client leaves charged.** The kernel makes a pager's object slots
256 at a time and keeps every piece for the pager's life (`kernel/pure/slots.rs`).
A freed slot is taken again, but its piece never goes back. A slot is 272
bytes: a 248-byte entry and two 12-byte marks, one for each notice it can
owe. Of the entry, the object's own state is 152 bytes, the pager's record 64
and the list of a mapped object's mappers 16; the walk state mapping needs
is 48 of the object's 152. Every entry carries all of it. Measured by the
compiler for both kernel targets, against 216 on main before mapping:
https://github.com/ToyOSOrg/evidence/tree/94fd400ddab5d9c2ff776ac9df66f1b0bb88b768/r12/sizes
So the server's job stays charged 272 bytes for each `/tmp` file of the most
it held at once, until the server ends. A client that makes and removes a
million files leaves about 272 MB. The manual has unused backing return under
pressure; this never returns.

Owner: the file server and the kernel's paged objects, once the owner has
decided the charge below.

**Exit**:

- **The charge, proposed and the owner's to decide.** What a server keeps for
  one client is charged to that client's job: a pager's object, and the pages
  supplied to it, are charged to a job the client's grant names, which the
  kernel lets the server charge by that authority. This changes the manual's
  rule that services own their private state. The alternatives are a
  per-share allowance in the server, which is a fixed part again, and leaving
  the charge with the server.
- **The slots.** A piece whose every slot is free goes back to the heap, and
  its charge with it. The free list takes the lowest free slot first, so the
  highest pieces drain. A host test of `Slots` shows a table that held many
  slots and let them go holding one piece's charge.
