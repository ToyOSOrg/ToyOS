---
status: open
kind: tooling
opened: 2026-10-09
---

# `acpi_hold` gives up at a time counted from boot, whatever its runner was given

`tests/toyos-rust-tests/src/bin/acpi_hold.rs` holds `testcases` open until
`/system/bin/acpiserver` has logged its count, which `acpi_server_events`
judges. Its wait ends at `UNTIL_MS`, `toyos_tco::JOB_BOUND_MS` less a tenth:
54 000 ms, counted from the kernel's zero and not from the hold's own start.
Two things follow.

**What the hold is given shrinks with every job before it.** It is the last of
its boot's rows' jobs. On the T14 at `accbd79dd` it started 43 161 ms in, with
10.8 s left; on the merged `testcases` at `2c7e1be1a`, 43 095 ms in, with
10.9 s left. A rows' job that adds 10.8 s to that list leaves it nothing:
`served_log::Log::until` then panics before it reads a line, with `the log did
not show … within 0ns`, and `acpi_server_events` reds over a count line that
was written some 20 s earlier and is in the log. The red names the server's
count, not the list that grew.

**The constant is not the bound its runner was given.** `UNTIL_MS` is written
as "the runner's bound less a tenth", and `testcases`' runner is given
`--bound-ms=100100`: the 60 000 ms and what its 225 members add. The members
were ordered behind the hold for this reason and no other. Behind them it
would start 51.8 to 52.5 s in by the readings on record, with 1.5 to 2.2 s
left.

## Owner

The harness: `tests/toyos-rust-tests/src/bin/acpi_hold.rs`, and
`TESTCASES_HELD` in `tests/toyos.rs`, which places it.

## Exit

The hold is bounded from its own start, or by the bound its runner was given,
and a job added before it cannot red `acpi_server_events` over a line the log
holds.
