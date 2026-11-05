---
status: open
kind: track
opened: 2026-09-05
---

# A user is a home tree and a login row

A stage of `issues/storage-is-layers-and-a-role-is-a-filesystem.md`;
where a program's own data goes depends on it.

To build: users as their home trees on DATA and their login rows in the
manifest. Until this lands there is one user, `toy`, whose home init makes at
boot.

Stage 3 of `issues/every-program-sees-only-the-files-it-was-given.md`, and blocked on its first two stages: a user is isolated only
once a session's view is all it can name. The mount protocol and real bcachefs
change what backs `/home`, not what a user is. Constraints a builder would
otherwise re-derive: the
sshd key path already has the login-row shape and is the first consumer; the
ROOT image stays immutable, so the user list lives on DATA.
