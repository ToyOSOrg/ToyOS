---
status: open
kind: tooling
opened: 2026-10-08
---

# `acpiserver`'s lines are spelled again by their readers

`lib/acpiserver-api` holds some of the server's lines (its count of the
controller's queries, its word on the controller, its idle and power
censuses, the first take of a query or GPE, and the end of its start), and
the server writes them through it and their readers match there. Every other
line of the server's that a reader matches is still a second spelling, held
to the server's by nothing: a change of wording in `system/acpiserver/src` is found as a red
guest test, or on the T14 as a wait that runs to its bound.

- `tests/common/power.rs`: `ACPI_ARMED`, `ACPI_PRESSED`, `ACPI_TABLE`,
  `ACPI_S5_HANDED`.
- `tests/toyos.rs`, `acpi_events_on_metal` and `acpi_tables_on_metal`: the
  armed line, `tables loaded in`, the tables' read counts, `refused`, and the
  `acpiserver: GPE ` and `query` words around the first take.

Owner: `system/acpiserver`.

## Exit condition

Each of those lines is a constant or a format of `acpiserver-api` that the
server writes through and its readers match on, and `rg '"acpiserver: '
tests/` finds no literal.
