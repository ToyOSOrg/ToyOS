---
status: open
kind: defect
opened: 2026-09-27
---

# A file on DATA is refused at 250 runs while the volume has free blocks

The DATA volume (`system/fileserver/src/data.rs`, over the `bcachefs` crate) keeps a
file's extents in its one btree entry, and one entry is at most
`MAX_ENTRY_SIZE` bytes (`lib/bcachefs/src/btree.rs`): 250 runs for `home/a`
(`file_entry_fits`, `lib/bcachefs/src/fs.rs`). On a volume whose free blocks are
all apart every run is one block, so a write that takes a file past 250 blocks
(1 MiB) is refused `ResourceExhausted` while the volume still has free blocks.
`SPREAD` lengthens the runs of files that grow in turn; it does nothing for
free space that is already fragmented.

Owner: the file server's DATA volume.

Evidence: `a_write_its_entry_could_not_name_is_refused_and_every_accepted_one_kept`
(`system/fileserver/src/data.rs`) is refused at `pages == 250` on a volume with
free blocks.

**Exit**: a file's runs held outside its one entry, so that test writes past
250 runs.
