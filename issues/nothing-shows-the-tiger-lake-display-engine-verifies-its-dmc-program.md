---
status: open
kind: defect
opened: 2026-11-01
---

# Nothing shows the Tiger Lake display engine verifies its DMC program

The tree carries Intel's Tiger Lake DMC package unmodified, pinned by hash
(`system/tgl-display/firmware/`, `NOTICE`). `tgl-display` loads its main
program into the display engine's CSR program storage through the iGPU's own
registers. Root `CLAUDE.md`'s firmware rule lets it ship: it is device
firmware, and what the display engine masters is behind the iGPU's VT-d
domain.

Nothing on record shows that the engine verifies the program before running
it:

- Vol 12-12.21 "DMC Firmware Package" marks the CSS header's `keySize`,
  `modulusSize` and `exponentSize` "Not used".
- The pinned file's CSS header carries 0 in all three. Its CSS `size` covers
  the whole 19,760-byte file as header, package header and programs, which
  leaves no room for a key or a signature.
- No Vol 12 or Vol 2c text states a check.

So, as far as the record shows, the program is unsigned. The hash pin keeps
an altered file out of a signed image, but it decides nothing about what the
engine accepts from a driver that hands it one.

**Owner**: the orchestrator.

**Exit**: an Intel statement, cited in the driver, that the engine verifies
the program before running it, and this file deleted.
