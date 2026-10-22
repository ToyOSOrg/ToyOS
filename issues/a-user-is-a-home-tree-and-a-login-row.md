---
status: open
kind: track
opened: 2026-09-05
---

# A user is a home tree and a login row

A stage of `issues/storage-is-layers-and-a-role-is-a-filesystem.md`;
where a program's own data goes depends on it.

A user has private storage, exposed as `/home/<user>` under
`issues/where-everything-lives.md`, and a protected userland login record from
which the supervisor builds the session's view and grants. Authentication and
storage-key bindings belong in protected userland records too; their schema is
not settled. There is no numeric user id checked by the kernel or ambient
"current user" that grants authority. A session holds its home because the
supervisor supplied that directory capability; programs receive only their
authorized grants. Until this lands there is one user, `toy`, whose home the
supervisor makes at boot.

Stage 3 of `issues/every-program-sees-only-the-files-it-was-given.md`, and blocked on its first two stages: a user is isolated only
once a session's view is all it can name. The mount protocol and real bcachefs
change what backs `/home`, not what a user is. Constraints a builder would
otherwise re-derive: the
sshd key path already has the login-row shape and is the first consumer; the
ROOT image stays immutable, so the user list lives on DATA.

## Authentication, unlock and session lifetime

Passwords are allowed; prefer secure passwordless login where supported. Entirely
passwordless enrollment requires a recovery route. Offer a randomly generated
recovery key; password-based setup may explicitly skip saving one. No automatic
administrator escrow. Export of a shared-volume recovery key requires explicit
system authority beyond ordinary login; it does not bypass a locked volume.
Biometric templates must not be treated as reproducible encryption keys.

Homes are directories on shared encrypted personal storage. Every regular user
may unlock it after reboot with their enrolled method; filesystem permissions
still separate users. Independent per-user encryption and shared-folder key
management are deferred. The storage service owns encryption and key lifetime;
authentication authorizes unlock. Once unlocked, storage remains available until
shutdown/restart, including screen lock, sleep and sign-out.

Switching desktop users leaves their applications running and requires normal
authentication on return. Signing out ends that session's apps/helpers, not
system services or separate sessions. Allow cancellation or explicit force when
work is unsaved or unresponsive. Programs without windows remain ordinary apps;
there is no separate background-work permission. Session/device transitions and
pre-unlock identity storage remain engineering design work.
