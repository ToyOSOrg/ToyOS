---
status: open
kind: tooling
opened: 2026-10-08
---

# `machine_shutdown_short_stop` asserts a program's line it never waited for

`machine_shutdown_short_stop` (`tests/common/power.rs`) waits for the
kernel's record `Q35_S5_SUPPLIED` and then asserts, on the console captured
to there, `acpiserver`'s `ACPI_ARMED`. The server says the armed line first,
but the two reach the console by different roads: the kernel writes its own
record, and a program's line arrives when `logkeeper` has read the server's
ring and relayed it. A capture that ends at the kernel's record does not
have to hold the server's line.

Seen once, beside `machine_shutdown` and `acpi_power_button` in one filtered
run, on a host at load 51 (1 min) running other worktrees' builds:

```
FAIL machine_shutdown_short_stop: "acpiserver: armed: power button served, embedded controller none" never reached the boot:
```

The capture holds the kernel's record, stamped 15.521 s, and no program's
line stamped after 15.175 s. Alone, two
minutes later at load 45 on the same tree, it passed in 12 s. The branch it
was seen on changed no line of `tests/common/power.rs`, and of `acpiserver`
only the spelling of its count line.

Owner: the harness, `tests/common/power.rs`.

## Exit condition

The test waits for `ACPI_ARMED` itself, as `acpi_power_button` does, before
it asserts it, and passes beside other guests on a loaded host.
