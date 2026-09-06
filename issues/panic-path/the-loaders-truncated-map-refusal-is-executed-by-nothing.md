---
status: open
kind: tooling
opened: 2026-09-06
---

# The loader's truncated-map refusal is executed by nothing

`start_kernel`'s copy of the UEFI memory map now refuses to grow its vector
after `ExitBootServices` — growing it takes a null pointer from an allocator
that is gone, and the panic that follows has no channel — and seals what it
dropped into the black-box page for the next boot to report
(`bootloader/src/blackbox.rs`'s `seal_loader_refusal`).

Neither the refusal nor that channel is executed by any test. QEMU's map fits
with room to spare: measured 2026-09-06 on the dev host, 108 descriptors at the
sizing and 101 in the map handed over, against a margin of 64. So the branch
that matters on the T14 — the only machine whose map has ever been suspected of
not fitting — runs nowhere.

What is owed is a loader boot parameter that sets `MAP_MARGIN` to zero, and a
two-boot judge in the shape of `panic_blackbox_survives`: the first boot drops a
few descriptors and seals the refusal, the second reports it. The loader already
parses its own parameters (`toyos_tco::PARAM` is one), so the arming has a home.

Filed rather than built because `bootloader/src/blackbox.rs` is about to be
rewritten around a loader-owned boot chain, and a judge written against the
current shape would be rewritten with it.
