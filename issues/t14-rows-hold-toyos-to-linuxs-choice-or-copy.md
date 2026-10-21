---
status: open
kind: defect
opened: 2026-10-10
---

# T14 rows hold ToyOS to Linux's choice or copy

`reviewer.md`'s **Tests**: a policy another OS chose is never an oracle for what ToyOS's own
setting must be, and its copy of a fact a primary source states is never an oracle. Two metal
rows in `tests/toyos.rs` used one. `counters_on_metal`'s, the request Linux's governor chose,
went in #915; the other remains:

- `acpi_tables_on_metal` holds the number of definition blocks the server loaded to a
  constant per bench, `Bench::blocks` in `tests/toyos.rs`: the T14's 14, the count Linux
  reported loading, and the AMD bench's 17. The firmware's XSDT and FADT, which the kernel
  already reads, are the primary source of that count.

Owner: the orchestrator, which dispatches the reshape.

**Exit condition**: `acpi_tables_on_metal` holds the count to the definition blocks
the same boot's firmware tables list, and `Bench::blocks` is deleted.
