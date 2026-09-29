---
status: open
kind: tooling
opened: 2026-09-29
---

# `toyos-pcid`'s `counting-allocator` control is run by no gate

`toyos-pcid/Cargo.toml` declares `counting-allocator` as the negative control
its isolation tests must red under, and `oracle.rs`'s
`the_counting_allocator_produces_a_cross_space_read` exists only with it on.
No row of `src/ci.rs`'s `CONTROLS` names it and nothing in `src/` or
`.github/` passes it: `src/build.rs`'s `declared_model_controls` reads six
model manifests, and `toyos-pcid`'s is not one of them. Clippy compiles it
(`src/clippy.rs`'s `UNCONTROLLED`); nothing runs it.

Exit: a `CONTROLS` row with its verdicts, and `toyos-pcid/Cargo.toml` among the
manifests `declared_model_controls` reads.
