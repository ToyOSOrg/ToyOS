---
status: assigned
kind: defect
opened: 2026-09-29
---

# The kernel loads no AMD microcode

The kernel is to load CPU microcode signed by the CPU's maker, pinned by
version and hash the way vendor device firmware is (owner, 2026-09-30), and
does not embed it for every CPU (owner, 2026-10-14): the build splits each
pinned vendor file into one patch per CPU on ROOT, the loader hands the
kernel its CPU's, and the kernel loads that on every CPU. On `main` it loads
Intel's alone, and on an AMD CPU every CPU's line says ToyOS has no AMD
microcode loader.

Assigned to branch `toyos-microcode-files`, which carries the design above
and AMD's loader with it; it closes this issue once both benches' runs meet
the exit. AMD's container parser (`build/microcode/amd.rs`) and guarded
write (`kernel/src/arch/x86_64/microcode.rs`) were built clean-room in
#920 from a specification of facts of `amd_ucode_info`, as root `CLAUDE.md`
asks: one agent wrote the specification citing no implementation, another
built from it alone. The AMD bench's answers to the specification's open
questions were measured at ToyOSOrg/evidence `32c335ece`,
`metal/amd-bench/`.

**Met** by #886: on the T14 every CPU runs Intel's 0xbe for 0x806c1
platform 7, `current` or `loaded`; the T14 armed with `microcode-reseal`
completes its boot and reds the row on a `refused` line; every guest loads
nothing, for its CPU reports a hypervisor or a vendor ToyOS has no loader
for.

Owner: the orchestrator.

**Exit**: the `microcode` metal row is green on both benches with the
loader naming the patch it handed — on the T14 `intel/06-8c-01/80` and
every CPU running 0xbe; on the AMD bench `amd/17-60-01` and every CPU
`loaded` 0x860010d — and the AMD bench armed with `microcode-amd-flip`,
whose patch data has one byte flipped, completes its boot and reds the row
on a `refused` line: a refusal the CPU raises as `#GP` is the one the
guarded write has to survive.
