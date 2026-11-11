---
status: open
kind: finding
opened: 2026-11-11
---

# The SDK keeps a second `StackedImage` nothing loads with

`sdk/toyos/src/elf/layout.rs` has a `StackedImage::place` of its own, used by
`sdk/toyos/tests/elf_crafted.rs` and by nothing that loads an image. The
loader places the kernel with `toyos_kimage::StackedImage::place`
(`lib/kimage/src/lib.rs`), which now leaves a page between the image and its
stack for the stack's guard; the SDK's copy still starts the stack on the
image's next page.

**Exit**: the SDK's copy and its test are deleted, or a caller is named that
needs the placement without a guard.
