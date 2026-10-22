---
status: open
kind: track
opened: 2026-10-22
---

# SQLite and PostgreSQL run on ToyOS

SQLite, as first-release applications bundle it, and later PostgreSQL and
Nginx run on ToyOS with their own semantics.

**Ruled** (owner, 2026-10-21; decisions, not implementation claims):

- **SQLite uses its stock Unix VFS** through ToyOS libc, including the copies
  applications bundle; a ToyOS-specific SQLite VFS is not required. The file
  server and the SDK supply the native file, locking, mapping and durability
  contracts, and libc adapts them in userland; no Unix filesystem policy
  enters the kernel. Multiprocess WAL, locking and durable I/O are validated,
  not startup alone.
- **An unlinked open file lives on** until its last release:
  `issues/an-unlinked-open-file-answers-stale.md`.
- **PostgreSQL starts its processes by `EXEC_BACKEND` over native spawn**,
  with the adaptation in PostgreSQL's platform port. No `fork` and no
  address-space cloning are added for it.
- **A shared segment is placed by the spawner:** the parent installs the
  authorised shared object at a requested address in the child before it
  starts. An overlap is refused and reported as a startup failure; rights,
  range, protection and accounting are enforced; address randomisation is
  unchanged for other mappings. The exact interface is engineering's.
- **Identity:** `issues/libc-answers-uid-0-for-every-process.md`. PostgreSQL's
  ownership checks keep their security purpose under native grants.
- **Signals** are the process track's
  (`issues/a-childs-end-is-an-event-and-a-parent-takes-its-children-down.md`).
  PostgreSQL's own signal table is not approved in its details.
- **Scope:** PostgreSQL and Nginx may arrive after the first release, but
  constrain design now; SQLite for first-release applications is unaffected.

**Delegated** (planning, 2026-10-21, under the owner's delegation; open to
the owner's challenge):

- **Wake-ups between PostgreSQL's processes** are userland coordination over
  the existing wait and wake primitives: no lost wake-up, no busy polling and
  no kernel mechanism for PostgreSQL alone.
- **Launch** is the existing launch and service lifecycle: no `system.toml`
  row is required, no quit reason is added for a database, and native quit
  requests are honoured.
- **No version, connection target or patch-size cap** is a product
  requirement; engineering pins versions for feasibility work and reports the
  limits.

**What SQLite needs**: record locks the file server serves
(`issues/std-file-lock-answers-ok-and-locks-nothing.md`), unlink-while-open,
`O_EXCL` on `/tmp` and `st_nlink` 1 (both done), a sub-second clock
(`issues/userlands-wall-clock-has-whole-seconds-and-an-mtime-has-nanoseconds.md`)
and a directory `fsync`, which this track owns: SQLite syncs a directory after
the unlink that commits a transaction in its default journal mode, and
ignores only a directory it cannot open. WAL also needs coherent shared file
mappings (`issues/process-memory-in-4-kib-pages-still-copies-each-programs-text.md`).
`issues/toyos-builds-itself.md` orders those cargo's SQLite needs too.

**Exit**: SQLite's own test harness, built for ToyOS from upstream with the
stock unix VFS, passes its multiprocess WAL, locking and durability tests in a
guest; and PostgreSQL's regression suite passes against a cluster started by
the port.
