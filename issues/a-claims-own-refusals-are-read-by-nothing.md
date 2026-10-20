---
status: open
kind: tooling
opened: 2026-09-14
---

# A claim's own refusals are read by nothing

Refusals of `kernel/src/pcidev/mod.rs` that no test reaches:

- `ClaimError::Owned`, `Refusal::NoInterrupt`, `Refusal::CapsTruncated` and
  the domain's fault, whose guest readers the guest suite's cut moved to stage J
  of `issues/the-guest-suite-runs-only-what-no-cheaper-tier-reaches.md`;
- `ClaimError::Ambiguous`, a partition GUID two partitions carry (a PCI
  claim names one function by its arrival since #926);
- `ClaimError::KernelDriven`, a claim on a function one of this kernel's own
  drivers bound;
- `ClaimError::Exhausted`, a claim past the vectors, remapping entries or
  LPIs a machine has;
- every BAR refusal — `Refusal::BarUnsizable`, `BarMoved`, `NoMappableBar`
  and the withholding `BarPages`, whose rule `toyos_pci::pages` tests on the
  host;
- every bound `SYS_DEVICE_BAR_MAP` and `SYS_DEVICE_DMA_ALLOC` check, which is
  `bar_object`'s index and zero-length bounds and `dma_alloc`'s
  `MAX_GRANT_BYTES` and `MAX_GRANT_TOTAL`;

A one-field mutation of any of them — an index bound compared `<=` rather than
`<`, a window overlap accepted, a grant total never summed — leaves every arm
green.

Owned by whoever next adds a boot config whose own test binary holds a claimable
function: netd holds this machine's only claim, and a test that makes it fail
this way costs the machine its NIC for that boot. Exit condition: a guest arm in
which a test binary's claim is refused each of these ways, each one red against
a kernel whose corresponding check answers `Ok`.
