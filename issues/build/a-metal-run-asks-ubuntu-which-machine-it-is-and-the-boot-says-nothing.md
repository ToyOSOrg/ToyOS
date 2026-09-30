---
status: open
kind: tooling
opened: 2026-09-29
---

# A metal run asks Ubuntu which machine it is, and the boot under test says nothing

`src/metal.rs`'s `run` asks the operating system the T14 runs between boots
for `metaltimings::Machine::QUERY` over ssh before the flash, and writes the
answer into the readback's `boot.txt` under three keys (`VENDOR_KEY`,
`PRODUCT_KEY`, `BIOS_KEY`); `tests/common/metal.rs` reads it back with
`metal::machine`, and `metaltimings::Record::load` picks the machine's record
by it. Both T14 runs of `wt/toyos-metaltimings` named the machine this way:
`machine LENOVO 20W0003AMZ, BIOS N34ET71W (1.71 )`.

A resident runner, tests inside a ToyOS that stays up on the machine with no
Ubuntu and no reboot per test, has nothing to ask: the ssh read,
`Refusal::Machine`, the three keys and `metal::machine` all go with Ubuntu.
The record compares BIOS strings byte for byte, so identity has one reader:
two that trim differently fail every run as a firmware change.

## Exit condition

The boot names its machine: the loader reads SMBIOS type 1's manufacturer and
product and type 0's BIOS version from the UEFI configuration table before
`ExitBootServices` and writes them as one `loader.log` line, the judge reads
the machine from the readback's `loader.log`, the ssh read, `Refusal::Machine`,
the three keys and `metal::machine` are deleted, and every record under
`tests/metal/` names its machine in the loader's own strings.
