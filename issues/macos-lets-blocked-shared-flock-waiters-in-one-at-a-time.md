---
status: open
kind: tooling
opened: 2026-11-01
---

# macOS lets blocked shared flock waiters in one at a time, so the build locks poll

When an exclusive `flock` is released, macOS admits one of the shared waiters
queued behind it. Each of the others is admitted only when the shared holder
ahead of it releases, although a new non-blocking shared try succeeds at once.
This was measured on Darwin 27.0.0, across threads and across processes alike,
by a stand-alone [probe](https://github.com/ToyOSOrg/evidence/blob/b2a2e6ed4e4948703ab270566db21d3ead2f35c9/r1/probe/main.rs)
([output](https://github.com/ToyOSOrg/evidence/blob/b2a2e6ed4e4948703ab270566db21d3ead2f35c9/r1/probe/probe.log)).
A build holds its uses for its whole length, so a waiter blocked in the kernel
waits on another build's whole length, and forever on a use held to the end of
a run. That is how test runs waited 2611 s and more on a sysroot their own
process had made (#1145, whose negative controls block in the kernel and admit
1 of 8).

So `build/buildlock.rs` polls every lock (`poll`): it tries, then sleeps,
starting at 1 ms and doubling up to 50 ms. Its costs:

- up to 50 ms between a release and a long waiter's next try, on every host;
- no FIFO order among waiters of one mode. A new waiter, trying often, can
  overtake long waiters, and only the 6 h ceiling bounds how often. The
  artifact lock is the exposed one, with five waiters at once for over 100 s in
  a 12-wide run. Exclusive-before-shared order still holds, through `intent`.

**Exit**: one of these lets a wait block on an event again, with a timed join
or equivalent for its ceiling, and the poll go.
- An event-based primitive that admits every compatible waiter on every host
  the build supports, and keeps flock's release on holder death.
- macOS admitting every compatible shared waiter, shown by the probe.

**Owner**: the orchestrator, who assigns the build track.
