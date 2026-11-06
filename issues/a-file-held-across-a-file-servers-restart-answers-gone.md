---
status: open
kind: defect
severity: [data-loss]
opened: 2026-09-27
---

# A file held across a file server's restart answers Gone

A file id is good only on the connection it was answered on, and a server's
volume id is its pid (`system/fileserver/src/main.rs`), so a file held across
a restart answers `Stale` or `Gone` (`toyos::fs`, so std's `File` and a C
descriptor alike), and a program that does not open its path again loses
what it writes after. logkeeper does: a part its volume refuses or loses is
opened again and given the rest from its replay
(`system/logkeeper/src/store.rs`). Reopening by path is the hazard: a stick
swapped for another carrying a file under the same name has that file
rewritten past what this boot last synced to it.

On DATA a held file can be told from another put at its path since: bcachefs
never reuses an inode number (`next_inode`, A8.9). Reopening it by that
number waits on a volume identity that outlives a restart, which the pid is
not; ruled so by the coordinator, 2026-11-05. A FAT role has no such
number and keeps `Gone`.

**Exit**: a file held on DATA across its server's restart is reopened by its
volume's identity and its inode number, and a host test that makes a file at
a held file's path before the restart has the held one still name its own.
