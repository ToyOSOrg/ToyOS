---
status: open
kind: tooling
opened: 2026-10-08
---

# `acpiserver`'s lines are spelled again by their readers

`userland/acpiserver-api` holds one of the server's lines, its count of the
embedded controller's queries, and the server, `acpi_hold` and the
`acpi_server_events` judge read it there. Every other line of the server's
that a reader matches is still a second spelling, held to the server's by
nothing: a change of wording in `userland/acpiserver/src` is found as a red
guest test, or on the T14 as a wait that runs to its bound.

- `tests/common/power.rs`: `ACPI_ARMED`, `ACPI_PRESSED`, `ACPI_TABLE`,
  `ACPI_S5_HANDED`.
- `tests/toyos.rs`, `acpi_events_on_metal` and `acpi_tables_on_metal`: the
  armed line, `taken for the first time`, `tables loaded in`, the tables'
  read counts, `refused`.
- `tests/toyos-rust-tests/src/bin/counters_metal.rs`, `acpi_said`:
  `acpiserver: armed: `, `embedded controller none`, the first-query line.

Owner: `userland/acpiserver`.

## Exit condition

Each of those lines is a constant or a format of `acpiserver-api` that the
server writes through and its readers match on, and `rg '"acpiserver: '
tests/` finds no literal.
