---
status: open
kind: tooling
opened: 2026-09-29
---

# `toyos-pcid`'s negative control runs nowhere

`toyos-pcid/Cargo.toml` declares `counting-allocator`, the control that reverts
`PcidPool` to the counter that reissued a live tag. Nothing runs it:
`src/ci.rs`'s `CONTROLS` has no row for it, and `src/build.rs`'s
`declared_model_controls` does not read `toyos-pcid/Cargo.toml`, so
`every_model_control_is_run` cannot notice. The control still has teeth, run by
hand: `cargo test -p toyos-pcid --features counting-allocator` exits 101 with
`tests::two_live_address_spaces_never_share_a_pcid ... FAILED`.

**Exit:** the control is a `CONTROLS` row demanding that `FAILED` line, and
`declared_model_controls` reads every manifest in the host workspace rather than
a list, so a control declared in a crate the list forgot reds.
