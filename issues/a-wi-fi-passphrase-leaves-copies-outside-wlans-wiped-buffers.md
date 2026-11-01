---
status: open
kind: defect
opened: 2026-11-01
---

# A Wi-Fi passphrase leaves copies outside wlan's wiped buffers

`wifi connect` hands `wlan` a network's passphrase, and `wlan` hands the radio
driver the temporal keys it derives. Both programs keep their own copies in
buffers they wipe with volatile stores: `toyos_crypto::Secret` in `wifi` and in
`toyos-wlan-station`, and `FrameRx::wipe` on `wlan`'s receive buffers. Some
copies are made outside them:

- **The kernel's message frames.** A frame's bytes are queued in 4 KiB frames
  that `kernel/src/frame_bytes.rs` says are "never zeroed", and they go back to
  the allocator holding them. That includes a connect's passphrase on its way to
  `wlan`, and every key on its way to the driver.
- **Standard input's buffer in `wifi`.** std's `Stdin` reads through its own
  buffer, which keeps the typed line after `wifi` has copied it out.
- **The terminal's echo.** A passphrase typed at a terminal is shown on
  screen, because the terminal has no way to stop echoing. A test job that
  pipes the passphrase in shows nothing.
- **Copies the compiler makes**, as `lib/crypto/src/secret.rs` lists them:
  moves, spills and finished frames.

Reading any of these needs kernel memory, or the memory of the program at
either end. The passphrase also stays in `wlan` for as long as it is wanted.

- **Owner:** the orchestrator.
- **Exit:**
  - the kernel zeroes a message's frames when the message is taken or its
    queue ends;
  - `wifi` reads its line without std's buffer;
  - the terminal can be asked not to echo, and `wifi` asks it while it reads.
