---
status: open
kind: track
opened: 2026-09-24
---

# Every program sees only the files it was given

Owner ruling, 2026-09-24: isolation is non-negotiable, and the filesystem is no
longer the declared exception to the capability model. This reverses his
ruling of 2026-08-20 that kept the filesystem outside it. Today any
process opens, deletes or spawns any path outside `/state`, so there is no
boundary between two programs, let alone between two people. Of `/state` each
row holds only its own directory, so sshd's key list is reachable from sshd
and what it spawns directly (`issues/sshserver-authorized-keys-unprotected.md`).

Rulings that bound the stages: who starts programs, "Restrict", grants as
badges on a connection, "Adopt", and `/tmp` per session (owner, 2026-10-03);
the local desktop counts as a login session, "Desktop counts" (owner,
2026-10-04), a premise that holds while only the compositor, terminal, shell,
toybox and sshserver rows list `starts` and no `/apps` row does. Once stage
3's local greeter exists, the local desktop is a login session like any
other, with the same per-session isolation (owner, 2026-11-05).

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
   **Done** (#1181; the track's step 8): fileserver's `resolve` suite, red
   with the grant's root ignored, and `toyos::files`'
   `an_empty_view_names_nothing_but_an_empty_root`. The kernel keeps no
   working directory (#1182): a spawn states its child's in the child's
   `PWD`, a name the child resolves in its own view, so it reaches nothing
   that view does not hold. Left of this stage: a row the image
   declares is still minted the whole tree but `/state`, which stage 2
   narrows to the files its row declares.
2. **Every row declares its files.** `system.toml` states each program's view;
   the build refuses a row that names a path no role provides. The shell and
   terminal get the session's view, doom its `/apps` directory, and daemons
   their own state. **Exit**: the machine boots with every program in a
   declared view, and nothing still sees the global tree.
   The `/apps` slice is built: a grant carries a read-only or read-write
   access that the file server enforces on every request that would change
   what it holds, and a package launched from `/apps` is minted its own
   directory read-only and its own folder of the session's home read-write,
   `/system` read-only, its own `/tmp` (`toyos_manifest::app_temporary`, a
   directory of the machine's), and nothing else a file server serves
   (`toyos_manifest::Program::view`). Every row the image declares still sees
   the whole tree but `/state`, of which each row holds only its own
   directory where it keeps state (`service = true` or `state = true`: the
   daemons' own state is built); `/system` is read-only, `/log` and `/apps`
   are read-only to all but one row each, and the rest is read-write. The
   storage rows hold no directory at all (#ENDOWPR): a service the
   supervisor adopted for its own files is inherited by no child.
   Only the installer service writes `/apps` (owner ruling, 2026-11-05).
   Built: `/apps` is read-only in every declared row's view but the row
   that declares `installs = true` (`toyos_manifest::Program::installs`),
   which in the image is `pkg`'s. `pkg` is that service and not a client of
   one: a launch already gives a row its own view, and who may launch it is
   the `starts` that list it, so the authority is `images/system.toml`'s
   alone and no port, protocol or daemon was added. `launch pkg install
   <file>` installs; a `pkg` a shell spawns as a helper holds the shell's
   view and refuses by name. Judged on the host:
   `toyos_manifest`'s `only_the_installer_writes_apps`, red with the view
   ignoring `installs`, and the build's
   `only_the_installer_s_view_writes_apps` over every committed config, red
   with that and with the shell's row granted `installs`. **Its exit**: a
   shell's write under `/apps` is refused by the file server, and an install
   through the installer service still commits. The first's check is the
   metal `app_view` row's shell arm, in place of a boot with the shell's row
   granted `/apps` read-write (orchestrator, 2026-11-11): the shell's
   redirect into `/apps` is refused with the file server's read-only reason
   while the same redirect into `/home` writes, and the two are one volume
   and one server differing only in the grant, so the grant is what
   refused. That row is green on the T14, both lines in its log (#1203).
   Owed on a machine: the second, which is `pkg_install_gbae`'s
   (`issues/the-guest-suite-runs-only-what-no-cheaper-tier-reaches.md`,
   stage D), whose client launches `pkg`.
   `/dev/null` is a name only libc's `open` knows, answered with a kernel
   null handle (`SYS_NULL`): no view holds it, so `stat` of it and a program
   that does not open through libc find nothing. Whether views name it is
   this stage's.
3. **Sessions and users.** `issues/a-user-is-a-home-tree-and-a-login-row.md`
   on top of views: a login authority (sshd, and later a local greeter, which
   asks for the desktop's session the same way) holds
   a `login` right and asks init's `launcher` for a session, and init builds
   the session's view from the user's row: the user's home, `/system`
   read-only, and the session's own `/tmp`. The machine's SSH identity is sshd's own
   state, not a user's. There is no `root` user and no `/home/root`, and a
   single key list sits in the login authority's view. **Exit**: a program in
   one user's session cannot name a file in another's, and appending to the
   key list is inexpressible from any session.
4. **What is not a file.** netstack names a socket by an id any
   client can name, and memory and process count have no
   per-session bound, so one session can starve another. Both close before
   the stage exits. **Exit**: a hostile session can neither reach another
   session's connections nor deny it memory or processes.

## Known weaknesses

- **A login session's `/tmp` is the machine's.** A session ends with its
  owner once every app in it has ended (stage 5 of
  `issues/a-childs-end-is-an-event-and-a-parent-takes-its-children-down.md`),
  and its launcher's port with it, but what it wrote under `/tmp` stays for
  every other session to read. The exit is the per-session `/tmp`.

## Ordering

After self-update and partition claims, and before the browser: rendering
untrusted content in a program that can read every file is not production
grade. The internet client (DNS, TCP, TLS) may run beside it.
