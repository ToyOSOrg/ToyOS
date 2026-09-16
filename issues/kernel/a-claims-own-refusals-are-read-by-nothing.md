---
status: open
kind: tooling
opened: 2026-09-14
---

# A claim's own refusals are read by nothing

Four of the refusals `kernel/src/pcidev/mod.rs` raises are read back:
`ClaimError::Owned` by `pci_function_is_exclusive`, `Refusal::Untranslated` by
`iommu_virtio_platform`'s no-unit arm, the domain by `userdev_dma_fault`, and
`SYS_DEVICE_REG_READ`'s bound by netd's own `config_space_is_bounded`. These are
reached by no test in any tier:

- `ClaimError::Ambiguous`, a config naming a device this machine has two of;
- `ClaimError::KernelDriven`, a claim on a function one of this kernel's own
  drivers bound;
- `ClaimError::Exhausted`, a claim past `MAX_FUNCTIONS` slots;
- every window refusal — `Refusal::NoWindow`, `BarUnsizable`, `BarUnplaceable`,
  `BarResized` and `Dead`;
- every bound `SYS_DEVICE_BAR_MAP` and `SYS_DEVICE_DMA_ALLOC` check, which is
  `bar_object`'s index and zero-length bounds and `dma_alloc`'s
  `MAX_GRANT_BYTES` and `MAX_GRANT_TOTAL`;
- the withholding of the MSI-X table's own BAR on the *ordinary* path:
  `place_bars`'s `Some(index) == table_bar` (`kernel/src/pcidev/mod.rs`)
  mutated to `false` hands that BAR to the holder of every function that
  publishes a table, and every tier stays green. A holder that can write the
  table aims the device's message at any address the LAPIC decodes, and
  `msix_bar` is named as the boundary in the module header while nothing reads
  it back. A refused claim is not the arm that covers this: the refusal spends
  no BAR at all, so what is unread is the hand-over that succeeds.

A one-field mutation of any of them — an index bound compared `<=` rather than
`<`, a window overlap accepted, a grant total never summed — leaves every arm in
every tier green.

Owned by whoever next adds a boot config whose own test binary holds a claimable
function: netd holds this machine's only claim, and a test that makes it fail
this way costs the machine its NIC for that boot. Exit condition: a guest arm in
which a test binary's claim is refused each of these ways, each one red against
a kernel whose corresponding check answers `Ok` — and, for the withheld table
BAR, one that holds a function publishing a table and is told that BAR's size
is zero.
