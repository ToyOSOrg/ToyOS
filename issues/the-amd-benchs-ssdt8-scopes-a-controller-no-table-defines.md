---
status: open
kind: defect
opened: 2026-10-12
---

# The AMD bench's SSDT8 scopes a controller no table defines, and is refused whole

The AMD bench's eighth SSDT ends in `Scope (\_SB.PCI0.LPC0.EC0)`, and no
table of the bench defines `EC0_`: its controller is `EC__`. ACPI 6.5
§19.6.120 reads "The object referred to by Location must already exist in
the namespace", so the block breaks a rule of the specification, and
`toyos-aml` refuses it whole with `NotFound`, keeping nothing of it.

**Ruled** (owner, 2026-10-12, on #875 in comment 6121646183): "Refuse the
table whole", by name; SSDT11's two calls into it fail by name too.

What the refusal costs, measured outside the tree on the bench's dump
(#875's M1, part 7): cut before the `Scope`, the block loads seven more
objects, one for each of its methods, and nothing else changes; SSDT11's
`M009` and `M010` call two of them, `M038` and `M039`, and any evaluation
that reaches either is refused by name. Neither is reached by the server's
start (#875's check outside the tree, `ToyOSOrg/evidence` `pr-875`).

Owner: the AML track
(`issues/toyos-runs-the-machine-in-acpi-mode-and-interprets-its-aml.md`).

## Exit condition

The bench's firmware ships an SSDT8 whose `Scope` names an object its tables
define, read loaded on the bench's `acpi_tables_loaded` row with the
expectation in `tests/toyos.rs` moved to 17 of 17; or the owner rules that a
block is kept short of a term that breaks a rule, and the interpreter does
so with a host test of it.
