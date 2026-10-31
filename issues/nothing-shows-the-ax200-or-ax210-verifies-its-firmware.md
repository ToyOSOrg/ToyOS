---
status: open
kind: defect
opened: 2026-10-31
---

# Nothing shows the AX200 or AX210 verifies its firmware file

The tree carries Intel's iwlwifi firmware for the two radios unmodified,
pinned by hash (`system/ax2xx/firmware/`, `NOTICE`); W5 puts it in the image
with the `ax2xx` driver. That is the owner's ruling that each part's newest
linux-firmware build ships whether or not a vendor statement shows the card
verifies it, accepted on the trust model's basis: the radio is a device, its
DMA behind the IOMMU, so firmware it runs reaches only what its domain maps.
Nothing on record shows that a card verifies its file. The AX2xx interface specification, revision 1 (§1, "Whether each part
verifies its file", and §14 item 1, on `ToyOSOrg/evidence` `pr-876`), reads:

- no reference states that either part verifies its file's signature;
- the AX210 file's build name is `release/core86:signed_remote:b20dcd1f`,
  the AX200's `release/core74::563a6e92`;
- neither file carries any of the three secure-section record kinds Intel's
  definitions name (24, 25, 26);
- Intel's paging definitions count one CSS block beside 32 data blocks, and
  each image's first section is 1,656 bytes.

None of these is a vendor statement. Until one is, whether a radio would run
an altered file is unknown: the hash pin keeps an altered file out of a
signed image, and decides nothing about what the card accepts from a driver
that hands it one. A radio whose DMA is not behind an IOMMU would leave
that unknown uncontained.

**Owner**: the orchestrator.

**Exit**: an Intel statement, cited in the specification, that each part
verifies its file before running it, and this file deleted; or a reader's
specification revision showing that a part does not, which reopens the
ruling with the owner.
