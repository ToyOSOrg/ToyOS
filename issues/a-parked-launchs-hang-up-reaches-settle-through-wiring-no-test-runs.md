---
status: open
kind: defect
opened: 2026-10-10
---

# A parked launch's hang-up reaches `settle` through wiring no test runs

A launch that asks the person at the screen is parked in the supervisor until
its question is answered, and `toyos_manifest::consent::settle` decides its
next step: a caller that hung up withdraws it before any answer. That decision
is host-tested. What feeds it is not: in `userland/supervisor/src/main.rs`, the
loop's watch of the parked caller's connection (`TOKEN_PARKED_CALLER`), the
ready test that reads it, `Supervisor::advance_parked`'s two arms, and `heard`,
which turns a `FrameRx` step into a `consent::Heard`. A defect there starts a
launch whose caller has gone, with the folder the person answered; it grants
nothing the person did not give.

**Evidence**: no guest test can hang up a caller while its question is up.
It needs a caller in a session that holds the screen which ends while asking,
and each candidate fails:

- the compositor's launcher thread never hangs up;
- a shell's own spawn waits on the answer, and the prompt takes every key;
- a program a shell spawns directly holds no launcher, and its helper panicked
  for want of one when tried;
- a test binary cannot be an image row (`build_programs` builds rows from
  `userland/<name>`), and the build refused one when tried.

## Owner

Stage 2 of `issues/the-supervisor-is-host-tested-and-owns-the-stop.md`, which
keeps handles, spawns and the loop in `userland/supervisor`.

## Exit condition

A guest test in which a caller in a session that holds the screen ends while
its question is up: the supervisor says `withdrawn`, the package never starts,
and the prompt leaves the panel.
