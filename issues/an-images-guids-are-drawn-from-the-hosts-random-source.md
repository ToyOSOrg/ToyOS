---
status: open
kind: defect
opened: 2026-10-12
---

# An image's GUIDs are drawn from the host's random source

Two builds of one plan write different images, so ToyOS rebuilding itself
cannot reproduce the host's bytes. Every GUID an image carries comes from
`image::random_guid`, the operating system's random source, drawn again on
every `create_boot_image` call: it draws the log partition's, the ESP's, the
slot table's and each slot's boot and ROOT GUIDs, and writes the log's again
into the ESP's `/toyos/log.guid` and each slot's into the slot table; `lay`
draws the image's disk GUID. `designate_data_disk` and `install` draw a DATA
partition's GUID the same way, and `lay` a disk GUID for each, on the scratch
disk a guest keeps `/apps` and `/home` on and on the disk a test boots an
installed image from. Neither disk is an image, and this file does not hold
them.

Measured on six pairs of metal images, each pair one selection staged twice
by `cargo test --test toyos-build -- --metal --metal-readback <dir>` from one
clean committed tree in one checkout: each pair differs in 391 to 413 bytes, and
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
a floor (`build/signing.rs`). The loader finds the log volume by it
(`loader/src/loaderlog.rs`). Its black box takes it as the owner of a record
left in memory, and clears a record another image left rather than report it
as this one's (`loader/src/main.rs`, `loader/src/blackbox.rs`). Its attempts
record on the log partition names it, and one that names another partition
is refused (`loader/src/attempt.rs`, `lib/update/src/record.rs`). Whatever
replaces the draw still gives two images that differ two log GUIDs, and
decides what a second build of one image inherits from the first one's floor.
The floor's need ends at stage 3 of
`issues/the-loader-does-only-what-must-precede-the-handover.md`, which
deletes `floor::Scope` and names every floor for its key alone, and the
attempts record's at its stage 5, which replaces that record with the slot
table's tries. The black box's does not: stage 9 moves its check into the
kernel, still keyed on the log GUID.

**Owner**: `issues/toyos-builds-itself.md`, whose M4 exits on a guest build
of the image whose hashes match the host build of the same commit, which no
image whose GUIDs are drawn meets.

**Exit**: every GUID `create_boot_image` writes, the disk GUID `lay` writes
for it among them, is a function of the image's own inputs (its sections and
its key), and a host test builds one image twice and finds its partition
table, slot table and `/toyos/log.guid` identical.
