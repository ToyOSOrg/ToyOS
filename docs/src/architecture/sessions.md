# Sessions and permissions

```text
   supervisor ─┬─ system services and drivers
               └─ session owner (one per login session; its launcher)
                     ├─ desktop or remote shell, and their helpers
                     └─ apps launched to outlive their launcher
```

## Session owners

- One per login session, started by the supervisor. It owns its desktop or
  terminal tree and the apps detached from it, and is the session's launcher.
- There is no separate session or launcher daemon.
- A session owner's failure may end its session, never another session or a
  system service.

## Helpers and launches

- **A helper** is an ordinary spawn. It inherits only what its parent passes
  it and lives in its parent's subtree and job. Running an installed app's
  code as a helper does not acquire that app's saved permissions.
- **A launch** asks the session owner to start an app under its own identity,
  with its own saved decisions and grants, outliving the launcher.
- A program may launch only the programs its manifest row lists; the caller's
  row is the approval. A direct spawn never inherits a launcher.
- Detaching gives a longer life, never more access. Jobs, identity and
  lifetime are distinct.

## Users

- A user is a home tree and a login row. There is no password file, no ambient
  "current user" and no root user.
- No numeric id is authority. libc maps the authenticated identity to user and
  group ids for programs that ask.
- A login authority, such as the SSH server or a local greeter, holds the right
  to ask for a session. The session's view is the user's home, the system
  read-only and the session's own temporary directory.
- A program in one user's session cannot name another user's files.
- The local desktop is a login session like any other: the local greeter asks
  for it, and it has the same per-session view and isolation. SSH
  authentication starts the user's session, whose shell inherits no host key
  and no SSH service authority.

## Ending a session

- Sign-out asks each of the session's apps to quit
  ([Processes and authority](authority.md)). An app that never listened for
  such a request ends at once. The others receive it, and the sign-out waits
  for them, reporting what remains, until it is cancelled or forced. The
  session's subtree ends once its last app has.
- A dropped connection or closed terminal sends its tree a hang-up, reports
  what remains and for how long, and keeps waiting.
- **No timer ends a process.** A process that received a request and carries
  on is ended only by an explicit force: a sign-out's force, scoped to the
  user's own sessions, or the system controls' stop. No request is repeated on
  a timer. A second one, which ends a process that has not yet taken the
  first and asks again one that has, is sent only by a person's action or
  the machine's stop.

## Permissions

- Authentication, authorisation and releasing an encryption key are separate
  duties. Consent approves a bounded action; an administrator's approval does
  not give every child of that administrator system power. No shell or AI
  agent is special.
- The permission service keeps saved decisions, asks for consent through the
  trusted interface, and authorises fresh grants. It is off the path of
  routine access: a granted capability works without asking it again.
  Granted capabilities survive its crash, and its parent restarts it.
- A file grant is an unforgeable badge on a connection, written by whoever
  minted the connection; the kernel vouches for what was granted, never for
  who connects.
- Session-sensitive capture and sharing are enforced by the resource service
  that owns them, and transfers cannot bypass the session's controls. Ending a
  capture is different from changing the policy for future grants.
- What any installed app may be granted is bounded by the image's ceiling,
  which names rights, never apps, so installing an app never edits the image.
- One authenticated app identity ties together installation, independent
  launch, saved permissions and private credentials. An immutable version
  identifies the code being run. A display name or an executable path is never
  authority.
