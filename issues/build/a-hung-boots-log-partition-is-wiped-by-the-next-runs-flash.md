---
status: open
kind: tooling
opened: 2026-09-14
---

# A hung boot's log partition is wiped by the next run's flash, before anyone reads it

`src/metal.rs`'s `Driver::flash` (`:1000-1002`) opens every run with
`self.as_root("wiping the old signatures", Job::Wipe, …)` followed immediately
by `self.as_root("flashing the stick", Job::Flash, …)` — `wipefs --all` then
`dd` over the whole disk, unconditionally, before that run's own boot has
happened. `run` (`:1416`) calls `driver.flash(&image)` at `:1471`, and only
after the reboot does it read anything back: `ride_the_reboot` at `:1483`,
`wait_for_the_stick` at `:1487`, `read_log` at `:1489`.

**So a run's own log is read only if that run comes back.** `wait_for_the_stick`
refuses `Refusal::Stick` (`:1107`) and the underlying `wait` refuses
`Refusal::Silent` (`:1121`) — both return before `read_log` (`:1489`) is ever
reached. A run that hangs writes nothing to disk that its own invocation goes
on to read, and the *next* invocation's `flash` destroys the partition before
anything else touches it.

## What this cost

Runs 36, 52, 54 and 55 (`lancase`-family, `t14-run*/` logs) hung past their
420 s deadline (`Refusal::Silent`); run 49 wedged the stick instead
(`Refusal::Stick`, back at 303 s). All five never reached `read_log`, and every
one of the five was followed by another run whose own `flash` — `wipefs` then
`dd` — overwrote the partition before it was ever read from outside the loop.
**No hung boot's own `/log` has ever been read.** That is the single largest
recoverable gap in diagnosing why the T14 does not come back: the record a
`logd` batch would have committed up to the instant of the hang (durability is
per-batch `fsync`, `userland/logd/src/main.rs:58`) is destroyed by the loop's
own next step rather than by anything the hang did.

## What would answer it

`read_log` already exists and already tolerates the stick being unreachable
(it is called after `wait_for_the_stick`). What is missing is a call to it — or
to the raw-sector read `raw_log` already takes for `--fat32-check`
(`run`, `:1489`-area) — **before** `flash`'s `wipefs`/`dd` at the *start* of the
next run, so the previous run's partition is read (and saved, where
`--readback` names a directory) before it is wiped rather than never.

**Exit condition**: `toyos-metal` reads and saves the stick's existing log
partition before `flash` wipes and overwrites it, so a run that hangs leaves a
partition the very next invocation captures instead of destroys.
