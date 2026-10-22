---
status: open
kind: tooling
opened: 2026-09-08
---

# Nothing reaches the MSI refusal arms of a claimed function

`pcidev::bring_up` arms a claimed function on MSI where it publishes no MSI-X.
The T14's I219 is one: `claim_reuses_its_remapping_entry` arms it through
`arm_claimed_msi` and releases it through `tear_down`'s MSI arm. No T14 row
reads a message it raised
(`issues/the-host-cannot-reach-the-t14-while-it-runs-toyos.md`). Nothing reaches
`Refusal::MsixUnusable`, owed only by a function that publishes MSI-X this
kernel cannot arm.

Owned by the network track's stage-2 I219 worker. Exit condition: a function
whose MSI-X cannot be armed is refused by `MsixUnusable`'s reason.
