---
status: open
kind: track
opened: 2026-09-24
---

# Every program sees only the files it was given

Owner ruling, 2026-09-24: isolation is non-negotiable, and the filesystem is no
longer the declared exception to the capability model. This reverses question
2 of `issues/kernel/the-capability-end-state-is-twelve-answers.md`. Today any
process opens, deletes or spawns any path, so there is no boundary between two
programs, let alone between two people: any process can append a line to
sshd's key list and log in remotely (`issues/isolation/sshd-authorized-keys-unprotected.md`).

## The model

The unit of isolation is the program. A user is the part of the tree a session
was handed, so isolating people follows from isolating programs.

- **A process has its own view of the tree.** `/` is per process, built by its
  parent: init builds every program's view from its `system.toml` row, the way
  it already builds the program's service namespace. Paths keep working, and a
  path names something only if it lies inside the caller's view.
- **A child's view is at most its parent's.** Nothing widens a view after
  spawn, and nothing in it is inherited wider than the parent held it. The
  swap port leaking to whatever sshd spawns (PR #484's review) is the same
  shape of defect in the service namespace.
- **Resolution cannot leave the view.** `..` at a view's root, a symlink,
  a rename racing a lookup, or a held directory handle never resolves outside
  the view. This is the classic way a restricted root is escaped, so it is
  built into the resolver by construction and tested adversarially, not
  checked afterwards.
- **Sharing is granted, never reached.** A file outside a program's view
  reaches it only through the file picker, which hands over that one file.
- **The special cases go.** `/boot` is in the updater's view and nobody else's.
  With that, the mount guard is deleted. Login data sits in the login
  authority's view alone.

Unix permission bits and numeric user ids are rejected: they leave every path
nameable by every program and make confused-deputy bugs structural.

## Stages

1. **The resolver.** A per-process view object in the kernel, the resolver
   confined to it, and `SYS_OPEN`, `SYS_READDIR`, `SYS_DELETE`, `SYS_MKDIR`,
   `SYS_RMDIR`, `SYS_RENAME`, `SYS_SYMLINK`, `SYS_READLINK`, `SYS_SPAWN` and
   `SYS_DLOPEN` resolving inside it. `SYS_SPAWN`'s working directory is one of
   those paths: `SpawnArgs` names it, and it must lie in the child's view or the
   spawn is refused. Today it is judged at spawn time only: the directory can
   be removed or replaced before the child starts, and the child then holds a
   path that names nothing — a view-relative cwd has to close that or say so.
   This lands as an ABI change on its own PR.
   **Exit**: an escape suite (every `..`, symlink and rename race the
   literature names) is red on a resolver that walks the global tree and green
   on this one. A process given an empty view names nothing.
2. **Every row declares its files.** `system.toml` states each program's view;
   the build refuses a row that names a path no role provides. The shell and
   terminal get the session's view, doom its `/apps` directory, and daemons
   their own state. **Exit**: the machine boots with every program in a
   declared view, and nothing still sees the global tree.
3. **Sessions and users.** `issues/filesystem/a-user-is-a-home-tree-and-a-login-row.md`
   on top of views: a login authority (sshd, and later a local greeter) holds
   a `login` right and asks init's `launcher` for a session, and init builds
   the session's view from the user's row: the user's home, `/system`
   read-only, and a private `/tmp`. The machine's SSH identity is sshd's own
   state, not a user's. There is no `root` user and no `/home/root`, and a
   single key list sits in the login authority's view. **Exit**: a program in
   one user's session cannot name a file in another's, and appending to the
   key list is inexpressible from any session.
4. **What is not a file.** netd names a socket by an id any
   client can name, and memory and process count have no
   per-session bound, so one session can starve another. Both close before
   the stage exits. **Exit**: a hostile session can neither reach another
   session's connections nor deny it memory or processes.

## Ordering

After self-update and partition claims, and before the browser: rendering
untrusted content in a program that can read every file is not production
grade. The internet client (DNS, TCP, TLS) may run beside it.
