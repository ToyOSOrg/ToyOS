---
status: owner
kind: question
opened: 2026-09-26
---

# Whether `bcachefs/` is ours to license is the owner's to rule

`bcachefs/Cargo.toml` declares no licence, and the kernel and the bootloader
link the crate, so every image ships it. It implements upstream bcachefs's
on-disk format, and upstream is GPL-2.0. Whether this crate is a work of its
own that the tree may license `MIT OR Apache-2.0`, or a derivative of
upstream's GPL source, depends on how it was written. A provenance audit is
running. Until the owner rules, `src/licence.rs` holds the crate and
`bcachefs/tests/fixtures/crc32c.img.gz` as named exceptions pending him. The
fixture's `COMMITTED_FILES` row and its `NOTICE` section declare `NOASSERTION`
rather than the tree's own licence.

**Exit**: the owner rules. If the crate is ours, its manifest declares
`MIT OR Apache-2.0`, the fixture's row and `NOTICE` section say the same, and
both exceptions are deleted. If it is not ours, it is relicensed, rewritten, or
leaves the image.
