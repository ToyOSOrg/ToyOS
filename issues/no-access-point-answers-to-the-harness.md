---
status: open
kind: tooling
opened: 2026-10-12
---

# No access point answers to the harness

Four Wi-Fi behaviours have no instrument (#876 §9, item 9, and W8 of
`issues/there-is-no-wifi.md`):

- an access point deauthenticating the station;
- an access point rekeying its group on demand;
- beacon loss: an access point the harness silences;
- roaming: two access points of one network, the harness silencing the one
  the station is on.

Each needs access points that the harness commands. Nothing in the tree
names one: `git grep -i 'access point' ab655a115 -- tests build system lib
kernel` finds nothing and exits 1. A host program that runs one is declared
under Arrivals, in
`issues/the-build-runs-host-tools-outside-rust-and-qemu.md`.

Owner: the orchestrator, whose benches they are.

**Exit**: a T14 row, and once W7 has landed an AMD bench row, in which the
harness makes an access point deauthenticate the station, rekey its group and
fall silent, and silences one of two access points of one network. In it the
station rejoins, installs the new group key, recovers from the silence as W8
decides, and roams to the other access point.
