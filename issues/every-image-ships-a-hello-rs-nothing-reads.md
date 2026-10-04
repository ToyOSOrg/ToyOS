---
status: open
kind: defect
opened: 2026-10-04
---

# Every image ships a hello.rs nothing reads

`assets/hello.rs` is swept into ROOT with the rest of `assets/`: the image
`cargo run -- --build-only` makes holds it as `share/hello.rs`, 55 bytes, by
`cargo run --example imgstat -- target/bootable.img`. No source, test or
manifest in the tree names it but its row in `src/licence.rs`, and that row
says `tests/common/compile.rs` builds it, which that file does not.

**Exit:** `assets/hello.rs` and its licence row are gone, or something that
runs reads it and the row names that.
