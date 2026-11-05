---
status: open
kind: defect
severity: [security]
opened: 2026-11-05
---

# A lend taken back before a release is not residue

`pcidev::dma_unmap` takes a lent region out of its function's domain and
keeps no record of the address. `tear_down` builds a function's residue from
the grants still live at its release, so an address that was lent and taken
back earlier in the claim is not residue, even if the function was still
aimed at it.

The scenario:

1. Claim A lends a region at device address X. The function queues a
   transfer to X.
2. A takes X back (`SYS_DEVICE_DMA_UNMAP`).
3. A is released, and nothing resets the function. That is the case for a
   virtio function on q35's root bus, or any function with no FLR and
   `No_Soft_Reset` set.
4. Claim B lends a region of its own, and `Window::lend_room` places it at X,
   since X is not residue.
5. B starts the function, and A's stale transfer lands in B's region.

Another process may still read that region. That breaks `pcidev`'s
guarantee that a lent region is never where a stale transfer lands. Main has
the same gap; #1188's review found it (sighting on #1188).

**Owner:** `pcidev`, on the small-kernel track.

**Exit:** a claim's taken-back lends become residue at a release that does
not reset the function. They must stay lendable again within the same claim,
which is authority that holder already has, or a holder that lends and takes
back repeatedly would empty its window. The likely shape is a per-claim list
of taken-back spans in `kernel::claimspace`, carved by later lends and added
to residue at a non-reset release. A host test in `claimspace` should
reproduce the scenario above and red without the fix.
