---
status: open
kind: defect
opened: 2026-09-24
---

# The kernel's generator is keyed once at boot and never reseeded, and a source is checked only for a constant

`kernel/src/random.rs` keys a ChaCha20 generator (`toyos-random`) once, before
the first hash container, from the seed the loader read from firmware's
`EFI_RNG_PROTOCOL` and from the CPU's own sources: `RDSEED` and `RDRAND`, or
`RNDR`. The hash seed and every `SYS_RANDOM` byte descend from that key. What
is still owed:

- **Nothing reseeds.** Each draw replaces the key, so the generator's memory
  read later gives no earlier draw; but a key read once gives every later draw
  until the machine restarts. Nothing mixes a fresh draw in on a schedule or
  after a resume.
- **The sources are firmware and the CPU, and nothing else.** There is no
  jitter source. A guest under HVF has no CPU source, so its key is whatever
  the host's generator gave QEMU's virtio-rng when edk2 read it, once.
- **The health test is one comparison.** A source whose 32 bytes are four
  equal words is refused by name: that is the all-ones `RDRAND` some AMD parts
  returned after a resume, and a source stuck on one value. A source that
  repeats with a longer period, or is biased, is mixed.
- **A key's copies outside the named buffers are not wiped.** The generator,
  a draw's stream and every block buffer are zeroed with volatile writes; what
  the compiler spilled of them to a stack frame or left in a register is not.

**Exit**: the generator is reseeded on a schedule from every source the
machine has, a jitter source among them. At boot and at each reseed a health
test refuses a source that repeats or is stuck, by name. A host test feeds a
reseed a stuck source and shows it refused and the key it had kept.
