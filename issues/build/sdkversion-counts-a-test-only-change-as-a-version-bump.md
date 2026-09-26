---
status: open
kind: tooling
opened: 2026-09-26
---

# `sdkversion::judge` counts a change under a published crate's `tests/` as a reason to bump it

`sdkversion::judge` (`src/sdkversion.rs`) walks every path under a published
crate's directory that changed since the merge base and asks
`identity::builds_differently` of each. It does not distinguish `tests/` (or a
path-only dev-dependency) from the crate's own `src/`: neither ships in what a
dependent resolves or links, so a change confined to them changes nothing a
downstream build can see, yet the gate demands the same minor bump and the same
crates.io release as a change to the published code itself.

Seen on #529: `toyos-ld/tests/...` changed with no change to `toyos-ld`'s own
`src/`, and the gate required 0.3.0 anyway.

The fix is a gate that knows which paths under a published crate's directory
can reach a dependent's build — `src/`, `Cargo.toml`'s non-dev sections, and
whatever `include`/`exclude` actually ships — and ignores the rest. Getting
that inventory wrong in the unsafe direction (calling something inert that
isn't) is worse than today's over-counting, so it wants care, not a quick
allowlist of `tests/`. The exit condition is a branch that touches only a
published crate's tests passing `sdkversion::judge` with no bump required.
