---
status: open
kind: tooling
opened: 2026-09-14
---

# A claim's own refusals are read by nothing

Refusals of `kernel/src/pcidev/mod.rs` that no test reaches:

- `ClaimError::Owned`, `Refusal::NoInterrupt`, `Refusal::CapsTruncated` and
  the domain's fault, whose guest readers the guest suite's cut moved to stage J
  of `issues/build/the-guest-suite-runs-only-what-no-cheaper-tier-reaches.md`;
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
  mutated to `(Some(index) == table_bar && false)` — the spelling that builds,
  where a bare `false` is `unused variable: table_bar` — hands that BAR to the
  holder of every function that publishes a table, and `https_tls13`,
  `https_tls13_e1000e`, `pci_function_is_exclusive` and `userdev_dma_fault` are
  each green on it. A holder that can write the
  table aims the device's message at any address the LAPIC decodes, and
  `msix_bar` is named as the boundary in the module header while nothing reads
  it back. A refused claim is not the arm that covers this: the refusal spends
  no BAR at all, so what is unread is the hand-over that succeeds.

A one-field mutation of any of them — an index bound compared `<=` rather than
`<`, a window overlap accepted, a grant total never summed — leaves every arm
green.

Owned by whoever next adds a boot config whose own test binary holds a claimable
function: netd holds this machine's only claim, and a test that makes it fail
this way costs the machine its NIC for that boot. Exit condition: a guest arm in
which a test binary's claim is refused each of these ways, each one red against
a kernel whose corresponding check answers `Ok` — and, for the withheld table
BAR, one that holds a function publishing a table and is told that BAR's size
is zero.
