---
status: open
kind: defect
opened: 2026-10-10
---

# T14 rows hold ToyOS to Linux's choice or copy

`reviewer.md`'s **Tests**: a policy another OS chose, or its copy of a fact a primary source
states, is never an oracle. Two metal rows in `tests/toyos.rs` use one:

- `counters_on_metal` holds every CPU's read-back `IA32_HWP_REQUEST` equal to Linux's
  (`tests/t14-linux/hwp-request.txt`). That value is the request Linux's governor chose, a policy,
  not a correctness truth.
- `acpi_tables_on_metal` holds the number of definition blocks the server loaded to
  `T14_DEFINITION_BLOCKS`, the 14 Linux reports loading. The firmware's XSDT and FADT, which the
  kernel already reads, are the primary source of that count.

Owner: the orchestrator, which dispatches the reshape.

**Exit condition**: `counters_on_metal` holds the request to the value the kernel declares, with
no reference to `hwp-request.txt`; `acpi_tables_on_metal` holds the count to the definition blocks
the same boot's firmware tables list, and `T14_DEFINITION_BLOCKS` is deleted.
