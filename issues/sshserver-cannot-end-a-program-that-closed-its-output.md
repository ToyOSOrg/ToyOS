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

Before #1026 the daemon polled `try_wait` with a backoff to keep the kill,
which is a fixed delay where an event exists.

A `cfg` reaching ToyOS's process handle, whose `wait` and `kill` take `&self`,
closes the hole on ToyOS. The host gate refuses that `cfg` in a program crate
that holds tests (`build/userlandhost.rs`) until #1024, after which a `cfg`
counts only in a file that holds a test or declares a module: the fix is a
ToyOS-only leaf file, as #1024's `system/sshserver/src/spawn.rs` is.

**Owner**: `sshserver`.

**Exit**: a program that closed its output pipes and runs on is ended when
its connection ends, read by a test that is red today.
