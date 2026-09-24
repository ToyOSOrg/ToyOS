---
status: open
kind: track
opened: 2026-09-24
---

# The machine updates itself, without Ubuntu, and boots only what the owner signed

Every kernel change on the T14 still needs Ubuntu to write the stick, and a
change to the machine's policy needs nothing more than a writable disk. The
owner's goal: Ubuntu is recovery only, updates are as fast as possible, and the
loader boots only images signed with the owner's key.

## The shape

- **Two slots, each a pair of partitions**: a FAT holding the kernel and its
  command line, and a bcachefs ROOT selected by `root=<uuid>`. The updater
  claims the idle slot's partitions by their unique GUIDs, as it would any
  device (`issues/boot-media/` partition claims, PRs #487 and #489).
- **The update rides the swap pipeline**: an authenticated ssh upload, the
  hash checked, and only the blocks that changed are written. A marker block is
  written last, after an fsync that answers for the updater's own writes.
- **The loader chooses.** It boots the marked slot and falls back to the other
  through the attempt counter it already has. A dead boot is reported on the
  next boot through logd. The boot order is set once. The loader itself is not
  slotted.
- **Signed images.** The Mac holds the owner's Ed25519 key and signs the
  kernel and ROOT of every image; the key never reaches the T14. The loader
  refuses a slot whose signature does not verify, by name, and falls back. The
  loader is the part signing does not cover: only UEFI Secure Boot over the
  loader closes that, and until then a writable ESP is the gap.

**Exit**: a kernel change reaches the T14 and boots with Ubuntu never started.
A slot with a flipped byte or no signature is refused and the other slot boots.
A boot that dies falls back on its own.
