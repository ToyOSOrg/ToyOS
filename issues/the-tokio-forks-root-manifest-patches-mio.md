---
status: open
kind: tooling
opened: 2026-09-29
---

# The tokio fork's root manifest patches `mio` onto the mio fork

`Cargo.lock` pins `ToyOSOrg/tokio` branch `toyos` at `d6c5691c`. Its
workspace root `Cargo.toml` adds
`mio = { git = "https://github.com/Japabu/mio", branch = "toyos" }` to
`[patch.crates-io]` (`git diff d7db722b d6c5691c -- Cargo.toml`; `Japabu/mio`
now redirects to `ToyOSOrg/mio`). A consumer resolves tokio through
`tokio/Cargo.toml` under its own `[patch]` — the root `Cargo.toml` names
`ToyOSOrg/mio` — so the entry acts only on a build inside the tokio
repository, and no upstream pull request could carry it.

**Owner**: the tokio fork.

**Exit**: the entry is off the `toyos` branch, and `Cargo.lock` is
re-pinned onto it.
