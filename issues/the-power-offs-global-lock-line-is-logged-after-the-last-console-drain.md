---
status: assigned
kind: defect
opened: 2026-10-08
---

# The power-off logs the Global Lock's give-back after the console's last drain, and `acpi_mediated_access` waits for that line

`acpi_mediated_access` (`tests/toyos.rs`) ends its wait on the kernel's line
`acpi: the Global Lock given back for a holder that left it taken (the machine
is stopping)`. Twice it never arrived, and the harness ended the guest after
`GUEST_QUIET` (15 s) of silence:

```
FAIL acpi_mediated_access: STALLED: waiting for the probe's power-off to give the lock back — it went quiet
```

| where | head | instrument | load | result |
|---|---|---|---|---|
| `main`'s nightly, run 37758546165, `tcg / suite` | `b6bcb9691` | TCG on a hosted 4-core x86-64 runner, 31 tests one wide | the suite alone | STALL after 18 s |
| the development machine, a whole-suite run of #763 | `252d19247` | TCG, x86-64 guest on an AArch64 host, 14 cores, 12 wide | load average 45 to 67: five agents compiling beside the suite | STALL after 35 s |
| `main`'s merge queue, six runs, #749 to #746 (37753990562, 37755528529, 37757410477, 37758981663, 37759626406, 37760845496), `guest / suite` | `6f87cdb9c` to `1084ddc9a` | KVM on hosted 4-core runners, 31 tests one wide | the suite alone | PASS, 3 s each |
| the development machine, `acpi_` by name | `252d19247` | TCG, 2 tests | run alone | PASS, 4 s |

The nightly's head has #749, which added the test, and nothing of #763: the
red is `main`'s. KVM on a hosted runner is not TCG on a loaded host, so the
six green runs say only that the test passes where the guest is fast.

## What was read

`power::shutdown` (`kernel/src/power.rs`) calls `serial::flush_final()` under
the comment "Last chance: nothing drains the log ring after this point", and
then `arch::power::off`. `off` (`kernel/src/arch/x86_64/power.rs`) calls
`acpi_mode::settle`, whose `give_back` logs the line the test waits for, then
`acpi_mode::quiet`, then writes `SLP_TYP` and `SLP_EN`. So the line is
committed after the stop's last drain. Past boot the console's one writer is
`klogd` (`kernel/src/log/console.rs`, `Drain::Thread`), woken at the commit: the
line reaches the host only if `klogd`, on the other CPU, puts it on the wire
before this CPU has made one port read, `quiet`'s writes and the two writes
that end the machine. Nothing waits for it.

That is a reading, and no run confirms it: neither red kept what the guest
said after boot. The test passes `await_guest`'s error up without its capture,
and the `uart-*.log` the nightly kept (artifact `serial-suite`, `uart-6.log`,
the `i8042-withheld` boot) ends where every boot's does, at the hand-over to
the virtio console. It fits both reds: the nightly's 18 s is the 3 s the test
takes under KVM and the 15 s of `GUEST_QUIET`. A probe whose arm failed would
have ended the wait by `===TEST_END`, and a kernel panic by its own line.

The branch that saw it locally moves nothing here: `settle`, `give_back`,
`off`, `shutdown` and the console are `main`'s bytes in #763.

Owner: the `acpi` claim's author (#749).

**Exit**: `acpi_mediated_access` green in the nightly's `tcg / suite`, with the
give-back's line on the wire before `SLP_EN` by construction and not by
`klogd` winning; and a stall of this test carrying what the guest said.

## What landed, and what is still owed

Everything above is `main` before #767 and is kept as it was written. #767
split the architecture's power-off in two: `arch::power::settle` says the
give-back's line, `power::shutdown` drains the console after it, and
`arch::power::off` takes the value only `settle` makes and logs nothing. The
exit's second and third clauses are met by that pull request: the order is
`kernel/src/power.rs`'s `shutdown`, and the test's stall appends what the
guest said since its boot.

The reading above has since been run. On one CPU no other CPU runs `klogd`
beside the power-off, and the line never arrived with the split reverted,
three boots of three, each capture ending at `Shutting down.`; with it the
line arrived, three of three. `acpi_lock_given_back_on_one_cpu`
(`tests/toyos.rs`) is that boot.

Its first clause is not met: no nightly has run `tcg / suite` on a `main`
that carries #767.

Assigned: the orchestrator, at #767's landing. He reads `acpi_mediated_access`
in the first nightly `tcg / suite` on a `main` that carries it, and deletes
this file on a green; a stall there prints the guest's words, and goes here.
