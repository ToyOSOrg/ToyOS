---
status: open
kind: defect
opened: 2026-10-22
---

# libc answers uid 0 for every process

libc's `getuid`, `geteuid`, `getgid` and `getegid` answer 0
(`sdk/libc/src/misc.rs`), and `stat` answers `st_uid` and `st_gid` 0, so every
C program believes it runs as root. PostgreSQL
refuses to start as root, and sshd and other daemons take root as a right
they hold.

**Ruled** (owner, 2026-10-21):
libc reports the process's actual ToyOS user identity, through one userland
mapping from the authenticated identity to user and group ids, and never
another invented constant. No Unix root authority enters the kernel:
capabilities remain the authority. Real and effective identity and file
metadata stay coherent, and an unsupported identity change fails honestly.
The identity it maps is `issues/a-user-is-a-home-tree-and-a-login-row.md`'s.

**Exit**: a C program run by a login reads that user's ids from `getuid`,
`geteuid`, `getgid`, `getegid` and the `st_uid` and `st_gid` of a file it
made, and `setuid` and `setgid` to another id are refused.
