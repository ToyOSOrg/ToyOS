---
status: open
kind: tooling
opened: 2026-10-02
---

# Pcidev issues cite cut guest tests as cover that runs

`520c0d129` deleted the guest tests `https_tls13`, `https_tls13_e1000e`,
`bar_placement_is_proven`, `pci_function_is_exclusive`, `virtio_net_no_msix` and
`userdev_dma_fault`, and three issues still give them as tests that run:

- `issues/kernel/a-claims-own-refusals-are-read-by-nothing.md`
- `issues/kernel/no-boot-reaches-the-refusal-that-keeps-a-bar-read-inside-a-declared-window.md`
- `issues/kernel/no-boot-reaches-the-retry-that-undoes-a-placement-nothing-answered.md`

One citation is cover no tier holds and no track owes. The last two give
`https_tls13_e1000e` as what reads `Refusal::BarReferenceEmpty`: its judge
asserted how many BARs the boot kept back, and that refusal's words. Outside
`kernel/` and `issues/` no file names the refusal, its words or the kept-BAR
record, and stage H of
`issues/build/the-guest-suite-runs-only-what-no-cheaper-tier-reaches.md` lists
that test for its fetch alone, on a line stage 3 of
`issues/design-debt/the-internet-clients-work-unchanged.md` deletes.

**Exit**: none of the three gives a test the tree lacks as one that runs, and
`BarReferenceEmpty` has a reader that reds when the refusal goes, or one of the
three lists it as unread.

Owner: the orchestrator.
