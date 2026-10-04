---
status: open
kind: tooling
opened: 2026-09-06
---

# The loader's truncated-map refusal is executed by nothing, and now says nothing

`start_kernel`'s copy of the UEFI memory map refuses to grow its vector after
`ExitBootServices` — growing it takes a null pointer from an allocator that is
gone, and the panic that follows has no channel. The refusal is correct and it
is the only thing standing between a firmware whose map does not fit and a
machine that freezes holding the loader's last line.

Two things are owed on it.

**It reaches no test.** QEMU's map fits with room to spare: measured 2026-09-06
on the dev host, 108 descriptors at the sizing and 101 in the map handed over,
against a margin of 64. So the branch that matters on the T14 — the only machine
whose map has ever been suspected of not fitting — runs nowhere. What would
execute it is a loader boot parameter setting `MAP_MARGIN` to zero; the loader
already parses its own parameters (`toyos_tco::PARAM` is one), so the arming has
a home.

**It also reports nothing.** It briefly sealed what it dropped into the
black-box page, and that page is gone: the loader's claim was removed whole
after the T14's firmware stopped returning from `ExitBootServices` with a
custom UEFI memory type in its map. A truncated map is now memory the kernel
silently never sees. The channel comes back when the page does, under an
ordinary memory type with its address on the kernel parameter line.
