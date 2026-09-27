---
status: open
kind: defect
opened: 2026-09-27
---

# Threads of one process take turns on one connection, and one can starve the rest

The std fork holds one connection per directory per process, behind a
`Mutex<toyos::fs::Dir>` (`rust/library/std/src/sys/fs/toyos.rs`,
`Capability`), and a call holds it across the whole request and reply. std's
mutex on ToyOS is the futex one, which lets a thread that lets go take the
lock again before a waiter it woke has run. A thread whose calls come back to
back can so keep every other thread of its process off the directory.

Seen on `quiesce_stops_the_machine` on a loaded dev host, one named test run
wide: `quiesce_writers`' six threads each create, write and fsync a file on
`/log` in a loop, and one of the six reached its loop in the test's 5 s; run
alone, all six did.

**Exit**: a thread waiting for a directory's connection is served in its turn
— a lock that hands over, or a connection per thread — with a guest test
whose threads each count their passes on one directory and none is starved.
