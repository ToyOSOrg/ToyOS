---
status: open
kind: defect
opened: 2026-09-29
---

# The kernel loads no AMD microcode

The kernel is to load CPU microcode signed by the CPU's maker, pinned by
version and hash the way vendor device firmware is (owner, 2026-09-30).
It loads Intel's (`toyos-microcode`, `kernel/src/arch/x86_64/microcode.rs`)
and no AMD CPU's: on one, every CPU's line says ToyOS has no AMD microcode
loader, and the `microcode` metal row holds the AMD bench to that line.

**Met** by #886: on the T14 every CPU runs Intel's 0xbe for 0x806c1
platform 7, `current` or `loaded`; the T14 armed with `microcode-reseal`
completes its boot and reds the row on cpu0's `refused` line; every guest
loads nothing, for its CPU reports a hypervisor or a vendor ToyOS has no
loader for.

**AMD.** AMD documents MSR0000_008B (55922) and not the patch loader, so
the loader rests on AMD's own parser, `amd_ucode_info`. Root `CLAUDE.md`
("Zero legacy") lets that source be read only clean-room: one agent writes
from it a specification of facts citing no implementation, and another
builds from that specification alone. #886's loader was not built that way
and was taken out.

The AMD bench's answers to the specification's open questions (M0–M6, M9,
M10) were measured by the scout at #914's `ca217141d` (`f7a07b1db`), its
reading at ToyOSOrg/evidence `32c335ece`, `metal/amd-bench/`; the scout was
taken out before #914 landed.

Owner: the orchestrator.

**Exit**: the kernel loads, early on every AMD CPU, the newest patch the
embedded container holds for it, and a CPU that does not take one says so
and runs what it reads back:

- the loader is built clean-room: one agent reads `amd_ucode_info` and
  writes from it a specification of facts of the container format citing no
  implementation, keeping the provenance in its own evidence, and another
  builds the loader from that specification alone, handed to it in its
  brief and never committed;
- a patch is matched through the container's equivalence table, CPUID.01H:EAX
  to processor ID and processor ID to patch, and a malformed table is refused
  by name; a host test whose table maps 0x00860f01 to a processor ID other
  than the one in a patch whose ID decodes to 0x00860f01 expects no match;
- every behaviour neither AMD's documents nor that specification states is
  named undocumented in the module header with the measurement behind it,
  or is not in the loader;
- the `microcode` row is green on the AMD bench with every CPU running the
  container's patch, and the bench with its trigger write removed completes
  its boot and reds the row on a `refused` line.
