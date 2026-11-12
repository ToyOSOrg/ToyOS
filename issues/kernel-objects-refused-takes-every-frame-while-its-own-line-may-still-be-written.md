---
status: open
kind: defect
opened: 2026-11-11
---

# `kernel_objects_refused` takes every frame while its own line may still be written

`tests/guest/rust-tests/src/bin/kernel_objects_refused.rs` promises: "No
filesystem service is asked to allocate while the machine has no room". But
it prints `taking every frame a process may` and commits every frame at
once. logkeeper carries that line to the Log file server, which may still be
writing it when the test holds every frame. If a process ends inside that
window, its memory comes back, and the test's later refusals are judged
against memory it did not take.

Seen in guest run 43092179151 (#1252 at 041f1b81e):
- `kernel_objects_refused_alone` failed `a pipe's first write, its frame with
  every frame taken` (`Ok(())`, expected `ResourceExhausted`).
- The pipe, port, event and null object before it were refused.
- In the same window the Log file server (pid 9) ended, and
  `logkeeper: /log has not answered (the file server is gone)` followed.
- pid 9 left no panic line in the job's capture. The serial artifact did not
  upload, so the kernel's account of its end is lost.
- So the mechanism is inferred, not established: a write the test caused,
  refused for want of a frame, ended the server.

The row passed at 2bbd17005 and 8f5cfaf65 of the same branch. Its result
depends on whether logkeeper's write lands before the commit, which the test
leaves to timing. The control, the same row at main e10b6fd49 in run
43102568233, passed. One green run cannot tell an intermittent race on main
from a defect of #1252: the case for the race rests on reading the test.

Owner: the guest test.

Exit: the test commits only once nothing it caused is still being written,
or it tells a premise broken by another process's end apart from a refusal
it failed to see. A red then names the cause, not the pipe's write.
