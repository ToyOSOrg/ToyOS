---
status: open
kind: tooling
opened: 2026-09-04
---

`forks.toml`'s `owed` fields (before this task's repin) named only cpal,
winit and softbuffer as pinning toyos-abi/toyos/toyos-window at "0.1"; empirically
`getrandom` (all three branches), `mio`, `socket2` and `libloading` — tier
`fork`, not `sibling` — also pin `toyos-abi = "0.1"` (`mio`/`socket2` also
`toyos = "0.1"`), each verified by reading the pinned commit's manifest from
`~/.cargo/git/checkouts/`. After repinning cpal/winit/softbuffer to 0.2,
`userland/Cargo.lock` still carries both `toyos-abi 0.1.0`/`0.2.0` and
`toyos 0.1.0`/`0.2.0` (registry-vs-path) because of these four; only
`toyos-window` reached one version, since winit and softbuffer were its only
consumers. Closing this needs the same branch-plus-repin treatment on
`ToyOSOrg/getrandom`, `ToyOSOrg/mio`, `ToyOSOrg/socket2` and
`ToyOSOrg/rust_libloading`.
