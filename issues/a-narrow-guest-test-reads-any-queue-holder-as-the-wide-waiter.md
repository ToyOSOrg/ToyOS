---
status: open
kind: tooling
opened: 2026-11-04
---

# `a_narrow_guest_does_not_overtake_a_wide_one_waiting` reads any holder of the queue as the wide guest waiting

`build/buildlock.rs`'s test starts the wide guest, then polls `guest-cpus/queue`
and starts the narrow guest once a `try_lock` of it fails, taking that as the
wide guest holding the queue while it waits. Under host load it failed with
`a narrow guest took the CPU a queued wide one waits for`. Its log shows both
guests *waiting for* the queue, the narrow one acquiring it first and the wide
one after, so the probe's failed `try_lock` was not the wide guest queued.
The run was `cargo run -- --ci host` at `2d29d98db` (#1173, a change that does not
touch `buildlock.rs`), host load average 22 to 28; the test passed alone three
times on the same tree: evidence `pr-1173/r1/r1-host-ci.log` and
`isolated-*.log` at ToyOSOrg/evidence `067de8d244ac7e8c517cf1039ec692f5241c097f`.

**Exit**: the test starts the narrow guest only once the wide guest itself
reports that it holds the queue, and stays green under load.
