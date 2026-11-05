# Sessions and permissions

## Who starts what

```text
   kernel
     └─ supervisor ─┬─ system services and drivers
                    └─ session owner (one per login session)
                          ├─ desktop or remote shell, and their helpers
                          └─ apps launched to outlive their launcher
```

The supervisor is the only program the kernel starts. It starts the system's
services and drivers from the system manifest and one session owner per login
session. A session owner owns its desktop or terminal tree and the apps
detached from it; it is also that session's launcher. There is no separate
session or launcher daemon.

## Helpers and launches

There are two ways to start a program, and they differ in authority:

- **A helper** is an ordinary spawn. It inherits only what its parent passes it
  and lives in its parent's subtree and job. Executing an installed app's code
  as a helper does not acquire that app's saved permissions.
- **A launch** asks the session owner to start an app under its own identity,
  with its own saved decisions and its own grants, and to keep it running after
  the launcher closes. A program may launch only the programs its manifest row
  lists; the caller's row is the approval. A direct spawn never inherits a
  launcher.

Detaching a program gives it a longer life, never more access. Jobs, which
share CPU time and memory accounting, are distinct from both identity and
lifetime.

## Users

A user is a home tree and a login row, and nothing else: no numeric user id
the kernel checks, no password file and no ambient "current user". A login
authority, such as the SSH server or the local greeter, holds the right to ask
for a session, and the supervisor builds the session's view from the user's
row: the user's home, the system read-only and the session's own temporary
directory. A program in one user's session cannot name another's files. There
is no root user. Unix permission bits and numeric user ids are deliberately
absent: they leave every path nameable by every program and make confused
deputies structural.

The local desktop is a login session. SSH authentication starts the user's
session, whose shell inherits no host key and no SSH service authority.

## Ending a session

Sign-out asks the session's apps to finish, allows cancel or force, then ends
the session's subtree. A dropped connection or closed terminal sends its tree a
hang-up, reports what remains and how long it has been outstanding, and keeps
waiting. **No timer ends a process.** Only an explicit force does: a sign-out's
force, scoped to the user's own sessions, or the system controls' stop. A
second quit request ends a process that ignored the first only when a person
or an explicit force sends it. A session owner's failure may end its session and never another session or a
system service.

## Permissions

Authentication, authorisation and releasing an encryption key are separate
duties. Trusted consent approves a bounded action; an administrator's approval
does not give every child of that administrator system power. No shell or AI
agent has special privileges.

The permission service keeps saved decisions, asks for consent through the trusted
interface, and authorises fresh grants. It is not on the path of routine
access: a capability already granted carries its access without asking it
again. Existing capabilities survive the service's crash, and its parent
restarts it like any service.

A file grant is an unforgeable badge on a connection, written by whoever minted
the connection; the kernel vouches for what was granted, never for who
connects. Session-sensitive capture and sharing are enforced by the resource
service that owns them, and transfers cannot bypass the session's controls.
Ending a capture is different from changing the policy for future grants.

What any installed app may be granted at all is bounded by the image's
ceiling, which names rights, never apps, so installing an app never edits the
image. Harmless rights, such as a window, sound and the app's own data folder,
are free; others come from a user action, like picking a file, or from consent
at run time.

One authenticated app identity ties together installation, independent launch,
saved permissions and private credentials. An immutable version identifies the
code being run. A display name or an executable path is never authority.
