---
status: open
kind: tooling
opened: 2026-09-14
---

# No boot reaches the retry that undoes a placement nothing answered

`kernel/src/pcidev`'s `place_bar` settles a candidate on `after == signature`:
the function answering at the new address what it answers where firmware put it.
On every machine in reach the first candidate answers, so that comparison has no
arm that reds when it is wrong — deleting it and accepting unconditionally
leaves every record byte-identical and `bar_placement_is_proven`,
`pci_function_is_exclusive`, `https_tls13_e1000e`, `virtio_net_no_msix` and
`userdev_dma_fault` all green.

What is therefore unexercised is the whole path under it: the `left {at:#x}`
record, the BAR written back to what firmware left in it, the address given back
to its run, and the next candidate tried. That path is what the module header's
"a placement that does not answer is undone whole" promises.

The tree has no way to produce a candidate that does not answer. Inside a
declared window q35 routes every address to PCI, so a BAR moved anywhere inside
one decodes there; the addresses that answer nothing — the platform's fixed
MMIO at `0xFEC00000` and up, a range no bridge forwards — lie outside every
declared window, and `toyos_pci::placement::reserve` cannot spell one.

What *is* covered: `toyos-pci`'s `probe::degenerate` and `aperture::decode` are
pure and tested, `a_released_address_is_offered_again` pins the give-back
arithmetic, and `https_tls13_e1000e` asserts the count of BARs kept back by
name, so deleting the `BarReferenceEmpty` refusal reds.

**Exit condition.** A guest whose claimed function sits behind a `pcie-root-port`
given a memory window of a few MiB. **The port's window is the thing to arrange**:
it produces the case only because `KernelArgs::root_bridge_windows` carries the
loader's free GCD MMIO ranges, which are the host bridge's whole aperture and not
the port's slice of it — on q35 that range is `0xc1100000+0x3af00000` — so a
candidate past the port's window is an address this kernel may name and the
function does not answer at. Booted with an arm asserting the `left {at:#x}`
record, that the BAR ended at a later candidate, and that deleting the comparison
reds that arm.
