---
status: open
kind: finding
opened: 2026-10-22
---

# The loader parses ROOT's filesystem to find the microcode patch

The kernel parses no filesystem of ROOT: ROOT is an opaque signed section the
supervisor's file server serves. The loader still does. `loader/src/microcode.rs`
mounts ROOT read-only with the `bcachefs` crate after the slot's header has
verified it, and reads the patch `toyos_microcode` names for this CPU out of
`/system/share/microcode/`, so a filesystem reader of ROOT's whole format runs
in the loader on every x86-64 boot. It is outside the kernel scope of the
owner's 2026-10-21 ruling and was left as it is.

Owner: the orchestrator.

The candidate exit: the image's patches as a signed boot section of their own
beside the kernel and the two boot programs (`toyos_update::image`), which the
loader picks this CPU's patch out of without reading ROOT, so ROOT is opaque to
every boot stage. It has to be weighed against
`issues/a-new-microcode-patch-reaches-a-machine-only-in-a-new-image.md`, whose
exit delivers a new patch as a file on ROOT.
