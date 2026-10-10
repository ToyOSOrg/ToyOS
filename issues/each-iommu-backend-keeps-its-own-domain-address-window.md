---
status: open
kind: defect
opened: 2026-10-10
---

# Each IOMMU backend keeps its own domain address window

Where a device domain's addresses start, where they end and which of them
are handed out is one policy — a floor a quarter of the way up what the unit
translates and above all memory, a ceiling under the first root-bridge window
or reserved region over the floor, an address never handed out twice — and it
has two declarations: `Domain::new`, `Domain::reserve`, `Domain::handed_out`
and `ceiling` in `kernel/src/arch/x86_64/vtd/table.rs`, and `FLOOR`,
`Addresses` and `ceiling` in `kernel/src/arch/aarch64/smmu/domain.rs`. Nothing
holds the two equal; each carries its own compile-time checks.

The SMMUv3 came in with the x86-64 side outside its fence, so its copy was
written beside VT-d's rather than hoisted above both.

**Owner.** `issues/toyos-runs-on-arm64.md`'s stage 6, whose ITS stage edits
`kernel/src/iommu/mod.rs` and both backends' seam.

**Exit.** One declaration above `kernel/src/arch/`, read by both backends:

```
rg -n 'fn handed_out|fn ceiling' kernel/src/arch
```

prints nothing.
