---
status: open
kind: tooling
opened: 2026-10-02
---

# Kernel issues cite the cut HTTPS tests as cover that runs

`520c0d129` deleted the guest tests `https_tls13` and `https_tls13_e1000e`, and
`issues/a-claims-own-refusals-are-read-by-nothing.md` still gives one or both
as tests that run: cover no tier holds and no track owes. Stage H of
`issues/the-guest-suite-runs-only-what-no-cheaper-tier-reaches.md` lists
`https_tls13_e1000e` for its fetch alone, on a line stage 3 of
`issues/the-internet-clients-work-unchanged.md` deletes.

**Exit**: no issue cites a test of either name that the tree lacks.

Owner: the orchestrator.
