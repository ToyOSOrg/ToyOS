---
status: open
kind: tooling
opened: 2026-09-27
---

# The harness carries three helpers nothing calls

`tests/common/mod.rs` puts `#[allow(dead_code)]` on nearly every module, so
the compiler says nothing about a harness helper whose last caller went. With
those attributes taken off, `cargo check --tests` on `wt/toyos-schedule` names
three that no test reaches, and none of the three was called by anything that
branch deleted:

- `tests/common/storage.rs`: `FileBlocks::whole`;
- `tests/common/qemu.rs`: the field `usb_images` and its method `usb_images`;
- `tests/common/qemu.rs`: `QmpDevices::set_link`.

The same pass names `tests/common/audio.rs`'s `completions` and `clients` and
`tests/common/stats.rs`'s `fisher_reject_at`, which the audio branch
(`wt/toyos-notiming`) deletes with their modules.

**Exit**: the three deleted, and the module-wide `allow(dead_code)` replaced by
nothing, so the next orphan is a warning the host gate denies. Owner:
orchestrator.
