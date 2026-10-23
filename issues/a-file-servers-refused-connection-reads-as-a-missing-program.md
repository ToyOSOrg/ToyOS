---
status: open
kind: defect
opened: 2026-10-23
---

# A file server's refused connection reads as a missing program

`toyos::files::is_served` (`sdk/toyos/src/files.rs`) answers `false` for any
path whose route fails, a refusal included: `toyos_fs_is_served`
(`sdk/toyos/src/files/owner.rs`) answers the route's error, and `is_served`
reads every answer but `Ok(1)` as "the kernel's". So when a process's first
connection to the file server serving a path is refused, std's spawn takes
the kernel's path for a program on that server, and the kernel, which serves
no `/system`, answers `NotFound`.

Seen on the T14 at `db6be8df1` and on `main` `35e930136`, `proctreecase`:
`process_tree`'s depth chain, each link a helper of the one before in the
job's share, stopped at link 31 with "its next link was refused: entity not
found", and the kernel logged `spawn: /system/bin/test_rs_process_tree: not
found`. SYSTEM's server serves one share at most `SERVED_SHARE` (32) clients
(`system/fileserver/src/bounds.rs`), and the 32nd process of that share to
connect is refused. The chain now runs from a copy on `/tmp`; the defect
stands for any program a share's 33rd process spawns off a file server.

Exit: a refused connection reaches the spawn's caller as the server's
refusal, never as the kernel's `NotFound`; a guest arm spawns a `/system`
program from a process whose share is at the server's bound and reads that
refusal.
