---
status: open
kind: defect
opened: 2026-10-01
---

# The console and `surface::Host::accept` wait on a readiness answer that may be spent

A poller's `READABLE` is a cue to look, not a promise that bytes are there: a
poll an earlier round left armed answers for a write a later round already read
(`process_watch` in `kernel/src/inbox/mod.rs` completes an already-ready handle
at once and leaves that poll registered, and `toyos::poller`'s capacity counts
both answers). The terminal and `toyos-window` read past such an answer without
waiting. Two readers still block on one:

- `userland/console/src/main.rs` reads the shell's stdout and stderr with std's
  blocking `read` on `TOKEN_STDOUT` and `TOKEN_STDERR`. A spent answer parks the
  console until the shell writes again, and a shell at its prompt is waiting for
  the keys the parked console no longer forwards.
- `toyos::surface::Host::accept` (`toyos/src/surface.rs`) calls the blocking
  `Acceptor::accept` on the acceptor reading ready, so a spent answer parks the
  terminal and the console until the next client connects. It is under
  `toyos/src`, so it is an ABI brief's.

Owner: `userland/console` and `toyos::surface`.

**Exit**: neither waits on a readiness answer — the console reads its shell's
pipes as the terminal does, and `Host::accept` takes a connection only when one
is queued.
