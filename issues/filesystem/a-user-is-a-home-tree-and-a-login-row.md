---
status: open
kind: track
opened: 2026-09-05
---

# A user is a home tree and a login row

Stage 3 of `issues/filesystem/storage-is-layers-and-a-role-is-a-filesystem.md`,
filed now by the owner's ruling of 2026-09-05 so the package track knows where
a program's own data goes.

A user is two things: `/home/<user>`, a directory on DATA holding Documents,
Downloads, `.config/<app>` and `.local/<app>`; and a row in the manifest from
which init builds a login session's namespace, the way it builds every system
program's from `system.toml`. Nothing else names a user: no numeric id the
kernel checks, no password file, no ambient "current user" a process can ask
for. A session holds its home tree because init moved that directory's handle
into it, and a program launched inside the session holds what the session's
launcher row grants it. Until this lands there is one implicit user and a
package writes under `/apps/<name>/` only.

Blocked on the hierarchy, which landed as #401, and on nothing else: the
mount protocol and real bcachefs change what backs `/home`, not what a user
is. Constraints a builder would otherwise re-derive: paths are ambient by the
owner's ruling (`issues/kernel/the-capability-end-state-is-twelve-answers.md`),
so a per-user home is a convention plus a handle, not a permission check; the
sshd key path already has the login-row shape and is the first consumer; the
ROOT image stays immutable, so the user list lives on DATA.
