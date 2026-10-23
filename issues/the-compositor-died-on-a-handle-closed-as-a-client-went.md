---
status: open
kind: defect
opened: 2026-10-23
---

# The compositor died on a handle closed as a client went

Once, in `session_lifecycle` on QEMU x86-64, the compositor was ended by the
kernel with `fault=handle`, one millisecond after it dropped a client.

The user's sign-out had asked the terminal to quit with a hang-up. The
compositor said `dropping client 8218 — its connection is gone` at 4.654.
Then, at 4.655:

```
handle fault: pid=5 tid=0 syscall=90 a handle closed at an earlier generation
```

Syscall 90 is `SYS_INBOX_SUBMIT`. The reading is that the main thread
submitted a poller watch on a handle the client drop had just closed. The
teardown around `system/compositor/src/client.rs`'s `Dead` and `mark_dead` is
the likely site. The number may be stale, though:
`issues/a-crash-report-can-name-a-syscall-that-already-ended.md` notes that
`process::handle_fault` prints a per-CPU syscall number with no bracket. The
fault was on cpu0, and the terminal's exit landed on cpu1.

The run was twelve guests wide while the host's disk was full. That boot had
already said "the file server is gone" before the sign-out, which may have
changed the terminal's exit timing. The same test passed alone straight
after, and in the same twelve-wide batch once the disk had room. The log is
`p1c/r1/qemu-in-the-disk-full-window.log` in ToyOSOrg/evidence at
`bd83bf4cc5c9d89a6e84102717ca34c96bc3d97f`.

**Exit:**
- Find the watch submitted on a closed handle, and make a dropped client's
  handle unreachable to every later watch.
- Add a host or guest case that drops a client while its handle is due to be
  re-armed. It goes red on today's code if the site is the one above.
