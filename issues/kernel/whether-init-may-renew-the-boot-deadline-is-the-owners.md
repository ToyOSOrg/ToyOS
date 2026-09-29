---
status: owner
kind: question
opened: 2026-09-29
---

# Whether init may renew the boot deadline is the owner's

`boot-deadline=` arms a bound from boot that nothing on the machine can hold
off (`kernel/src/deadline.rs`), so a boot meant to stay up for a session of
tests is reset at it. Stage 1 of
`issues/hardware/the-t14-reboots-through-ubuntu-for-every-test.md` needs it to
be a lease, and that is a new kernel call, so an ABI change:

- It renews the deadline to at most `WEDGE_BOUND_MS` from now, or retires it.
  Its right is init's alone: no manifest name grants it, so no row can hold
  it. test-runner asks init over a port the build lets only test-runner's row
  receive, and hands its jobs a namespace without that port.
- It is refused once `deadline::stand_down` has run
  (`kernel/src/deadline.rs:79-81`, called from `kernel/src/panic.rs:352`). A
  renewal that raced a panic would otherwise arm the deadline again and seal
  `WEDGED` over the panic report.
- A lapse is recorded as the deadline expiring, with when it was last
  renewed, and not as a wedge: a host that went away stops renewing as surely
  as a wedged machine does.

*Recommended: yes.*

**Exit**: the owner rules.
