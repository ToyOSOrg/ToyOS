---
status: open
kind: defect
opened: 2026-10-21
---

# sshserver reports a killed program as `EXIT_LOST`

A program a signal ended has no exit code, and `sshserver`'s `Program::wait`
(`system/sshserver/src/program.rs`) answers it as `EXIT_LOST` (254), "this
daemon cannot say how it ended". It can say: std's `ExitStatusExt::signal`
answers the signal on ToyOS as on Unix, and RFC 4254 §6.10's `exit-signal` is
the channel for it, which `toyos-ssh` does not send yet.

What stops it: `sshserver` builds for the host too, and reading the signal
there and on ToyOS takes a `cfg` naming each platform's `ExitStatusExt`, which
the host gate refuses in a program crate that holds tests
(`build/userlandhost.rs`, `every_userland_test_is_in_the_gate`).

Owner: `sshserver`.

**Exit**: a command killed over SSH answers the client `exit-signal` `KILL`, and
one faulting `SEGV`, read by a test that is red with `EXIT_LOST`.
