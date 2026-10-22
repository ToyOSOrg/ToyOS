---
status: open
kind: track
opened: 2026-09-24
---

# Every program sees only the files it was given

Owner ruling, 2026-09-24: isolation is non-negotiable, and the filesystem is no
longer the declared exception to the capability model. This reverses question
2 of `issues/the-capability-end-state-is-twelve-answers.md`. Today any
process opens, deletes or spawns any path, so there is no boundary between two
programs, let alone between two people: any process can append a line to
sshd's key list and log in remotely (`issues/sshserver-authorized-keys-unprotected.md`).

## The model

The unit of isolation is the program. A user is the part of the tree a session
was handed, so isolating people follows from isolating programs.

- **A process has its own view of the tree.** `/` is per process, built by its
  parent: init builds every program's view from its `system.toml` row, the way
  it already builds the program's service namespace. Paths keep working, and a
  path names something only if it lies inside the caller's view.
- **A child's inherited view is at most its parent's.** A program cannot widen
  its own authority; later access requires an authorized grant, such as a
  document selected through the trusted picker. Nothing is inherited wider
  than the parent held it. The swap port leaking to whatever sshd spawns
  (PR #484's review) is the same
  shape of defect in the service namespace. **A launch is the exception, and
  its caller's row is the approval** (owner, 2026-10-03, "Restrict": each
  program may start only the programs its configuration row lists; swap and
  update of system programs only from the login session): the launched
  program holds its own row, the supervisor refuses a row the caller's
  `starts` does not list, and a direct spawn never inherits a launcher.
- **Resolution cannot leave the view.** `..` at a view's root, a symlink,
  a rename racing a lookup, or a held directory handle never resolves outside
  the view. This is the classic way a restricted root is escaped, so it is
  built into the resolver by construction and tested adversarially, not
  checked afterwards.
- **Sharing is granted, never reached.** A file outside a program's view
  reaches it through an authorized grant, such as a picker selection or a
  transferable capability supplied by another program, never by naming an
  otherwise inaccessible path. Delegation follows
  `issues/permissions-and-capabilities-need-one-runtime-contract.md`.
- **The special cases go.** `/boot` is in the updater's view and nobody else's.
  With that, the mount guard is deleted. Login data sits in the login
  authority's view alone.

**Ruled** (owner, 2026-10-03):

- **Who starts programs, "Restrict"**: a program starts only the programs its
  own `system.toml` row lists, and a system program is swapped or updated only
  from the login session.
- **Grants, "Adopt"**: a file grant is an unforgeable badge on a connection,
  written by whoever minted the connection; the kernel vouches for what was
  granted, never for who connects. Permission changes follow the shared runtime
  contract in `issues/permissions-and-capabilities-need-one-runtime-contract.md`.
- **`/tmp`, "Per session"**: a session's programs share one `/tmp`, and each
  service and each app has its own. This amends the 2026-09-26 ruling of
  `issues/where-everything-lives.md` that `/tmp` is private per
  program.

**Ruled** (owner, 2026-10-04), **the local desktop, "Desktop counts"**: the
local desktop is a login session before a greeter authenticates anybody, so
`swap` and `update` run from its shell. His words: "You can run swap/update
from the desktop shell, as today. After #709, untrusted desktop programs no
longer hold the launch right anyway, so excluding the desktop would mostly
just block you." That premise holds while only the compositor, terminal,
shell, toybox and sshserver rows list `starts` and no `/apps` row does. The
compositor's `login = true` in `system.toml` is the whole of it. Whether the
desktop keeps that pre-authentication authority once stage 3's local greeter
exists remains design work. The finished-distro passwordless authentication
target lives in `issues/a-user-is-a-home-tree-and-a-login-row.md`; this
development allowance is not a production authentication rule.

Unix permission bits and numeric user ids are rejected: they leave every path
nameable by every program and make confused-deputy bugs structural.

## Remembered file access

The shared permission model and its approved delegation policy live in
`issues/permissions-and-capabilities-need-one-runtime-contract.md`.

Owner decision, 2026-10-16: selecting a document for an application remembers
approval to issue access across application restarts until the user removes
that approval. Already-issued access follows the shared permission-change rule. This
does not grant the containing folder or widen the selected access rights.
Bind the stored grant to the user and protected application identity, consistent
with the verified-update policy in
`issues/permissions-and-capabilities-need-one-runtime-contract.md`.

Owner decision, 2026-10-16: remember the authorized file location
under a trusted filesystem authority, rather than track a document's movement.
Renaming or moving the file requires a new selection to reopen it at its new
location, unless separately held scope already permits access. A different file
later placed at the approved location is also accessible with the approved
rights. This supersedes the earlier document-following and no-replacement
rules: no rename tracking or special permission transfer to an authorized
replacement. A path string alone is
still not authority, and resolving a grant must not allow symlink or mount
substitution to escape its authorized scope. Existing open handles retain
their object binding; they do not silently
retarget when the saved location changes contents.

Permission removal follows the owner's revised rule in
`issues/permissions-and-capabilities-need-one-runtime-contract.md`: refuse
fresh direct grants while leaving already-issued capabilities usable within
their scope. Removing a remembered file permission does not interrupt open
files or streams, or invalidate delegated copies. A retained directory or
location capability can still authorize operations within its original scope.
Stopping an app is a separate user action, not part of changing permission.

The current picker returns a path (`sdk/filepicker/src/lib.rs` and
`system/filepicker/src/main.rs`); that response alone does not establish a
persistent grant. The existing folder-grant store is not a selected-file grant
store. Secure reopening and replacement saves remain design
work. Dropping rename tracking removes document-following machinery; it does
not give an editor authority to create temporary files beside the selected
file. A document-only replacement needs scoped staging/commit support and
SDK integration; alternatively the app needs separately authorized folder
access. Do not infer that a location-based grant alone makes all existing
temporary-file-and-rename save patterns work unchanged. No persistent
document-identity registry is required for the remembered permission policy.

## Stages

1. **The resolver is the file server's** (amended by the owner's storage
   ruling of 2026-09-26, `issues/the-kernel-is-small-interrupts-post-and-threads-wait.md`
   stage 4; it was a per-process view object in the kernel). A view is a set
   of directory capabilities: init builds each from the program's row as a
   connector to a file server's port, and tells that server which subtree and
   which rights the port serves. The server resolves every path relative to the
   directory the connection is bound to, by node identity, in the one resolver
   that owns its node table — so `..` at that root, a symlink's target and a
   rename racing a lookup are refused there. The client library keeps the
   prefix-to-capability table and the working directory; an absolute symlink
   is resolved again in the client's own table, so it reaches nothing the
   client does not hold. The kernel's only part is who holds which connector,
   which it already enforces. `SYS_SPAWN`'s working directory becomes one of
   the client's directories, and a spawn names a directory the child is given,
   never a path judged at spawn time. Built as that track's step 8, over the
   file servers of its steps 6 and 7.
   **Exit**: an escape suite (every `..`, symlink and rename race the
   literature names) is red on a resolver that walks the global tree and green
   on the file server's. A process given an empty view names nothing.
2. **Every row declares its files.** `system.toml` states each program's view;
   the build refuses a row that names a path no role provides. The shell and
   terminal get the session's view, doom its `/apps` directory, and daemons
   their own state. **Exit**: the machine boots with every program in a
   declared view, and nothing still sees the global tree.
   The `/apps` slice is built: a grant carries a read-only or read-write
   access that the file server enforces on every request that would change
   what it holds, and a package launched from `/apps` is minted its own
   directory read-only and its own folder of the session's home read-write,
   and nothing else a file server serves (`toyos_manifest::Program::view`).
   It still reaches `/tmp` and `/system`, which the kernel serves to every
   program. Every row the image declares still sees the whole tree
   read-write, `/apps` included, so a shell and everything it starts can
   rewrite an installed package. Replace that access with protected committed
   versions and capability-authorized installation/activation as approved in
   `issues/a-package-is-a-directory-under-apps-and-the-installer-is-a-program.md`.
   Being a shell or installer grants no authority by itself.
   `/dev/null` is a name only libc's `open` knows, answered with a kernel
   null handle (`SYS_NULL`): no view holds it, so `stat` of it and a program
   that does not open through libc find nothing. Whether views name it is
   this stage's.
3. **Sessions and users.** `issues/a-user-is-a-home-tree-and-a-login-row.md`
   on top of views: a login authority (sshd, and later a local greeter) holds
   a `login` right and asks init's `launcher` for a session, and init builds
   the session's view from the user's row: the user's home, `/system`
   read-only, and the session's own `/tmp`. The machine's SSH identity is sshd's own
   state, not a user's. There is no `root` user and no `/home/root`, and a
   single key list sits in the login authority's view. **Exit**: a program in
   one user's session cannot name a file in another's, and appending to the
   key list is inexpressible from any session.
4. **What is not a file.** Connections require capability authority, not a
   guessable id. Charge induced kernel allocations and protect control/cleanup
   capacity. Applications share remaining memory without default per-session
   quotas; allocation may fail when another job consumes it. **Exit**: one
   session cannot access another's connections, and resource exhaustion leaves
   system controls usable to inspect and stop jobs.

## Known weaknesses

- **A login session never ends.** A program launched in one keeps the session
  in its launcher's badge after the login that opened it is gone, so a
  detached program can start `swap` after logout. The exit is a session's end
  ending its launches, which the per-session `/tmp`'s wait on the session's
  first process provides.

## Ordering

After self-update and partition claims, and before the browser: rendering
untrusted content in a program that can read every file is not production
grade. The internet client (DNS, TCP, TLS) may run beside it.
