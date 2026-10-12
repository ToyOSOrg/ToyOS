---
status: open
kind: defect
opened: 2026-10-12
---

# An image's GUIDs are drawn from the host's random source

Two builds of one plan write different images, so ToyOS rebuilding itself
cannot reproduce the host's bytes. Every GUID `build/image.rs` writes comes
from `gptwrite::random_guid`, the operating system's random source, drawn
again on every call: `create_boot_image` draws the log partition's, the
ESP's, the slot table's and each slot's boot and ROOT GUIDs, and writes the
log's again into the ESP's `/toyos/log.guid` and each slot's into the slot
table; `lay` draws the disk GUID of every disk it lays; `designate_data_disk`
and `install` draw the DATA partition's.

Measured on six pairs of metal images, each pair one selection staged twice
by `cargo test --test toyos-build -- --metal --metal-readback` from one clean
committed tree in one checkout: each pair differs in 391 to 413 bytes, and
kernel, loader, ROOT and LOG are byte-identical in all six. In one pair,
254 of its 393 bytes are the GUIDs and the GPT CRCs over them, 135 are the
clock
(`issues/an-images-signed-version-and-its-fat-dates-are-the-hosts-clock.md`:
the signed version and the signature over it, and FAT entry times), and 4
are the slot table's CRC over both.

Two checkouts of one commit differ further, and not by anything this file
holds: in the kernel and ROOT, as
`issues/two-checkouts-of-one-tree-build-different-guest-bytes.md` records,
and in the loader, which carries two copies of its checkout's throwaway
public key.

The log partition's GUID is what keeps a throwaway key's anti-rollback floor
per image: the floor's UEFI variable is named by it
(`lib/update/src/floor.rs`), so that no image the key signs holds another to
a floor (`build/signing.rs`), and the loader finds the log volume by it
(`loader/src/loaderlog.rs`). Whatever replaces the draw still gives two
images that differ two log GUIDs, and decides what a second build of one
image inherits from the first one's floor.

**Exit**: no GUID `build/image.rs` writes is drawn from a random source, and
a host test builds one image twice and finds its partition table, slot table
and `/toyos/log.guid` identical.
