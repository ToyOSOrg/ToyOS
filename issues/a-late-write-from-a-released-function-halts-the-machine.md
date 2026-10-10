---
status: open
kind: defect
opened: 2026-10-10
---

# A late write from a released function halts the machine

`kernel/src/pcidev/mod.rs`'s `tear_down` clears the function's ownership
(`crate::iommu::note_user_owned(…, None)`) before its reset has quiesced what
it already issued: Bus Master Enable is cleared first, so it starts nothing
new, and its grants stay mapped until it is quiet, so nothing it issued lands
in a page handed on. A transaction already in flight that faults after the
ownership is cleared — at an address a grant never covered, or one an
`unmap` before the release took back — is read by the fault handler as a
function's no process drives: `owner=kernel`, and `crate::iommu::fault::conclude`
halts the machine. One process's bug then takes the machine down, which is the
thing a claim exists to prevent.

This holds on x86-64 under VT-d today, and on AArch64 it is reachable once a
function can be claimed through an SMMUv3 domain. No test makes a write
outlive its function's release; QEMU's devices finish a DMA inside the access
that starts it, so no guest of this suite can.

**Owner.** `issues/toyos-runs-on-arm64.md`'s stage 6, its claim of a function
through an SMMUv3 domain (the stage's exit: netd claims its NIC), which makes
the AArch64 half reachable and lands before it.

**Exit.** A fault on a function between its release and its reset's quiet is
handed to the released claim's record and not to the kernel, the ownership
cleared only once the reset is quiet; a test that makes such a write — a
claimed function's grant unmapped while it is aimed there, its claim released
before the write lands — sees the record and a machine still running, and
reds with the ownership cleared where it is today.
