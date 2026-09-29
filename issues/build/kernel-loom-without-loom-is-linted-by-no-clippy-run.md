---
status: open
kind: tooling
opened: 2026-09-29
---

# `kernel-loom` without `loom` is linted by no clippy run

Both host shapes in `src/clippy.rs` build `kernel-loom` with its default
`loom`, so its other arm — the three `cfg(not(feature = "loom"))` sites in
`kernel-loom/src/lib.rs`, `tests/log_body_words.rs` and
`tests/log_zeroed_init.rs` — is compiled by no shape. A `mem::forget` planted in
the non-`loom` `percpu_fetch_add` leaves `cargo run -- --clippy` green.

`cargo clippy -p kernel-loom --no-default-features --all-targets -- -D warnings`
exits 101 with twelve findings: `missing_safety_doc` on that `percpu_fetch_add`,
and `new_without_default` on eleven kernel types' non-`loom` `const fn new`,
which `kernel-loom` exports `pub` and the kernel binary does not. Their `loom`
arms carry `#[allow(clippy::new_without_default)]`; these do not.

Exit: a shape builds `kernel-loom` without `loom`, and it is clean.
