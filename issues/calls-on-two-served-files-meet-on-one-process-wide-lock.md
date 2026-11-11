---
status: open
kind: finding
opened: 2026-11-11
---

# Calls on two served files meet on one process-wide lock

A call through a descriptor holds its object without a lock (#1234). A served
file's object is an index into `files::owner`'s `OPEN`
(`sdk/toyos/src/files/owner.rs`), one `Lock` over every served file the
process holds. `held()` takes it in every call on a served file
(`on_fd_held`) to clone that file's `Arc`, and `release_held` takes it when
the descriptor table lets one go. So two threads reading two different served
files still meet on one lock word each call, which is the collision the
descriptor table's hold no longer has. The critical section is short, so a
collision costs a brief wait. Found by inspection in the review of #1234;
nothing has measured it.

Owner: the SDK's served-file client (`toyos::files`).

**Exit**: a call on one served file takes no lock that a call on another
served file takes, or a metal measurement of two threads on two served files
shows the lock costs nothing material, with that measurement cited here before
the file is deleted.
