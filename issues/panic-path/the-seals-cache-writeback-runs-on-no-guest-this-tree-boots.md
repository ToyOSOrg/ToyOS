---
status: open
kind: tooling
opened: 2026-09-07
---

# The seal's cache write-back runs on no guest this tree boots

`kernel/src/blackbox.rs`'s `flush` walks the black-box page with `CLFLUSH` and
an `SFENCE` after every seal, because a reset invalidates the caches without
writing them back: a page sealed into write-back memory and then reset over is
a page whose bytes never reached DRAM, and it looks from the next boot exactly
like a seal that never happened.

**No test executes that, and none can on this instrument.** Every guest the dev
host boots is TCG, which has no caches for `CLFLUSH` to have anything to do, and
`pmemsave` reads guest RAM through QEMU's memory API rather than through a cache
hierarchy — so the judges that read the page back (`blackbox_fault_sealed`,
`blackbox_early_panic_sealed`) pass identically with the flush and without it.
The KVM shards run on real caches but assert nothing about this page.

What is owed is either a KVM-shard assertion that the page reads back across a
guest reset, or the T14 itself standing as the oracle: run 10 sealed PANIC and
the boot after it read `ARMED`, which is the shape a lost write-back has, and a
run that reports a panic after this landed is the measurement. Until one of
those, the instruction is carried by the SDM and by that one sighting.
