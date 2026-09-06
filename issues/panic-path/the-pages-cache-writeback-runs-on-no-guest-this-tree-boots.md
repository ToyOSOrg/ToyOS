---
status: open
kind: tooling
opened: 2026-09-07
---

# The page's cache write-back runs on no guest this tree boots

Every writer of the black-box page ends in `CLFLUSH` over it and an `SFENCE` --
the kernel's seals (`kernel/src/blackbox.rs`) and the loader's arm and clear
(`bootloader/src/blackbox.rs`) -- because a reset invalidates the caches without
writing them back. A page written into write-back memory and then reset over is
a page whose bytes never reached DRAM, and from the next boot that is
indistinguishable from a write that never happened.

**It has been both failures on the owner's T14 already**, which is the whole
evidence for the instruction: run 10 sealed `PANIC` and the boot after it read
the loader's `ARMED`; run 13 cleared the page after reporting and the boot after
it read the same report again off a freshly flashed stick.

**No test executes the write-back, and none can on this instrument.** Every guest
the dev host boots is TCG, which has no caches for `CLFLUSH` to act on, and
`pmemsave` reads guest RAM through QEMU's memory API rather than a cache
hierarchy. Measured 2026-09-07: taking the loader's `flush` out of the clear
leaves `blackbox_panic_chain` green, while taking the *clear* out reds it. So
what the judges hold is that each write happens, never that it lands.

What is owed is a KVM-shard assertion that the page reads back across a guest
reset, or the T14 standing as the oracle -- a run that reports its predecessor
once and then boots is the measurement.
