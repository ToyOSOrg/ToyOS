---
status: open
kind: defect
opened: 2026-11-01
---

# A Wi-Fi passphrase leaves copies outside wlan's wiped buffers

`wifi connect` hands `wlan` a network's passphrase, and `wlan` hands the radio
driver the temporal keys it derives. Both programs keep their own copies in
buffers they wipe with volatile stores: `toyos_crypto::Secret` in `wifi` and in
`toyos-wlan-station`, and `FrameRx::wipe` on `wlan`'s receive buffers. `wifi`
reads the passphrase from its descriptor itself, so std neither echoes it nor
keeps it (`issues/toyos-std-echoes-stdin-and-keeps-each-line.md`). Some copies are still made outside those buffers:

- **The kernel's message frames.** A frame's bytes are queued in 4 KiB frames
  that `kernel/src/frame_bytes.rs` says are "never zeroed", and they go back to
  the allocator holding them. That includes a connect's passphrase on its way to
  `wlan`, and every key on its way to the driver.
- **Copies the compiler makes**, as `lib/crypto/src/secret.rs` lists them:
  moves, spills and finished frames.

Reading any of these needs kernel memory, or the memory of the program at
either end. The passphrase also stays in `wlan` for as long as it is wanted.

- **Owner:** the orchestrator.
- **Exit:** the kernel zeroes a message's frames when the message is taken or
  its queue ends.
