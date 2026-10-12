---
status: open
kind: defect
opened: 2026-10-13
---

# The AMD bench's controller `_REG` and adapter `_PSR` are refused for a write to the root complex's index

On the AMD bench the server's evaluation of the embedded controller's
`_REG(3, 1)` and of the AC adapter's `_PSR` are each refused `ConfigWrite`:
both reach AMD's `ALIB` method in SSDT1, whose mailbox (`A011`) first writes
a DWord through a SystemMemory region over the ECAM window to function
00:00.0, offset 0xB8, and then reads and writes offset 0xBC as `BankField`s
selected by the value written there (ACPI 6.5 §19.6.7). The kernel refuses
every configuration write the claim's holder asks
(`toyos_userbound::firmware::config`).

**Ruled** (owner's orchestrator, 2026-10-12, #875 comment 6131125468): the
write stays refused. 00:00.0 offset 0xB8 is the root complex's index, and
through it AML reaches every register of the SoC, the IOMMU's configuration
among them; passing it, or every write past the standard header of a
function nothing claims, would put the server in the trusted base, against
"isolation is non-negotiable".

**What it costs**, read on the bench at #875's `855eff969`
(`ToyOSOrg/evidence` `pr-875`, `round3/amd-acpi_tables_loaded.log`, and
the terms by offset in `round3/amd-aml-chain.txt`):

- `_REG(3, 1)` is refused after it has stored `OKEC` and `BASC` and
  notified the battery, at its call of `^VPC0.NDPC`, so the census reads
  `no _REG run` and whatever `NDPC` would tell the firmware of the
  controller is not told;
- `_PSR` is refused at its `ALIB (One, …)`, so the adapter's state is
  absent: the battery reads `AC unknown`;
- each later evaluation that reaches `ALIB` is refused the same way. The
  battery's own `_BIX` and `_BST` are read.

**What AMD documents.** AMD's PPR for Family 17h Model 60h (55922-A1
rev 3.06) names 00:00.0, vendor 1022h device 1630h, the Root Complex
(§1.8.1, Table 13), the function the bench's kernel enumerates there, and
documents no register at its offsets 0xB8 or 0xBC nor anything the values
the mailbox writes to 0xB8 select: its register namespaces are the cores',
IO, MCA, SBTSI and SMU::THM alone (List of Namespaces; Memory Map - Main
Memory). So no documented register map exists yet to mediate that index by.

Owner: the AML track
(`issues/toyos-runs-the-machine-in-acpi-mode-and-interprets-its-aml.md`).

## Exit condition

A design the owner accepts that mediates writes through 00:00.0's 0xB8 and
0xBC by a register map AMD documents, built, with the AMD bench's
`acpi_tables_loaded` reading `_REG(3, 1)` run and `_PSR` answered, and the
expectation in `tests/toyos.rs` moved to it. Until then the refusal stays,
and the row expects it.
