---
status: open
kind: defect
opened: 2026-10-22
---

# sshserver cannot end a program that closed its output

`system/sshserver` ends every program a connection started when that
connection ends (`connection.rs`, `end`). It waits for a program's exit status
on a thread once both of its output pipes are at their end
(`program.rs`). std's `Child` takes the wait and the kill through one `&mut`,
so from then on that thread holds it. A program that closes its stdout and
stderr and keeps running is waited for, and its connection's end does not
end it. It runs until it exits on its own.

A program whose own child holds its pipes after it exits is not waited for
while they stay open; its channel's or connection's end then reaps it, and
the child runs on.

Before #1026 the daemon polled `try_wait` with a backoff to keep the kill,
which is a fixed delay where an event exists.

On ToyOS a channel's end no longer goes through the `Child`: it asks the
program to quit with a hang-up through a handle of its own
(`system/sshserver/src/spawn.rs`, `Ender`), and the connection's end signs the
session out, so a program that closed its output is asked all the same. One
that ignores the hang-up is the session's to end, by a force. The hole stays
on a host build, where the kill still goes through the `Child`.

**Owner**: `sshserver`.

**Exit**: a program that closed its output pipes and runs on is ended when
its connection ends, read by a test that is red today.
