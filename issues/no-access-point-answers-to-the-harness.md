---
status: open
kind: tooling
opened: 2026-10-12
---

# No access point answers to the harness

Two Wi-Fi behaviours have no instrument (#876 §9, item 9):

- an access point deauthenticating the station;
- an access point rekeying its group on demand.

Each needs an access point that the harness commands. Nothing in the tree
names one: `git grep -i 'access point' c0cdddf4e -- tests build system lib
kernel` finds nothing and exits 1. A host program that runs one is declared
under Arrivals, in
`issues/the-build-runs-host-tools-outside-rust-and-qemu.md`.

Owner: the orchestrator, whose benches they are.

**Exit**: a T14 row has the harness make the access point deauthenticate the
station and rekey its group. In that row the station rejoins and installs
the new group key.
