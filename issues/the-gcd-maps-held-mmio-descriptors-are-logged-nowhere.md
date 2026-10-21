---
status: open
kind: defect
opened: 2026-09-14
---

# The GCD map's held MMIO descriptors are logged nowhere

`loader/src/gcd.rs`'s `free_mmio` prints one line per free MMIO range it hands
the kernel and a count of the map's descriptors, and no line for any other
descriptor. So the map's held MMIO descriptors — firmware's *current*
allocations, the reading that lets that file's module header be checked against
a real machine — reach no console and no file. Eighteen of the ThinkPad T14's 75
descriptors are MMIO and five are logged; eleven of q35's 34 are MMIO and two
are logged.

What kept them out is gone. The loader's whole map, moved into `loader.log` on
`bar-placement` at `ffd3f9c2`, once cost the panel its kernel log, because
`/system/bin/console` seeded the screen from every `*.log` file in `/log`. It
now seeds from the boot `logkeeper` hands it over the log port
(`seed`, `system/console/src/main.rs`), so by reading, a line the loader writes
to its own file alone (`loaderlog::line`) costs no panel row; no boot has
checked that.

What pays for it is `toyos-metal`'s readback of `loader.log`
(`tests/metal/src/lib.rs`): a machine whose aperture is in neither channel
cannot have that premise checked against it afterwards.

**Exit condition.** The loader writes one line per GCD descriptor to its file
alone, and a T14 readback of `loader.log` carries the whole map.
