---
status: open
kind: tooling
opened: 2026-09-26
---

# An SDK bump leaves every fork on the last SDK, and nothing says so

`src/sdkversion.rs` bumps a published crate's minor on every change to it,
and every fork names the SDK crates by that minor (`toyos-window = "0.15"`,
`toyos = "0.13"`, `toyos-abi = "0.12"`). So the next change to
`toyos-window` leaves winit (both branches) and softbuffer resolving the
published 0.15 beside the tree's 0.16, and a change to `toyos-abi` does the
same to getrandom (three branches), mio, socket2, libloading and cpal. Cargo
builds that without a word: two copies of one ABI crate, the forks on the old
one. It happened once already — the forks sat on 0.1 and 0.2 while the tree
reached 0.12 — and was fixed by hand.

Exit condition: a gate that reds on a lockfile in the tree holding two
versions of a crate `sdkversion::PUBLISHED` names, so a bump lands with its
forks' repin branches or not at all.
