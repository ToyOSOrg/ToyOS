---
status: open
kind: defect
opened: 2026-09-27
---

# A held file is known across a file server's restart by what its entry records

A handle held across a restart of its file server reopens the file by path
and keeps it only when the server answers the identity the handle last saw
(`toyos::fs::Stat::ident`). The volumes have no object id to answer with: the
interim DATA format's entry is a name, a length, an mtime and extents
(`bcachefs/src/fs.rs`), and a FAT entry a short name, a creation stamp, a
first cluster and a length. So the identity is a hash of what the entry
records — on DATA the first block, the length and the mtime, which the format
keeps to the second; on FAT the short name, the creation stamp, the first
cluster and the length (`userland/fsd/src/volume.rs`, `Volume::ident`).

That tells a file renamed over the held one, a file made again at its path,
and a write the restart lost from the file the handle held. It does not tell
a replacement that takes the held file's freed first block — the allocator
hands back the most recently freed first — and has the same length, written in
the same second: the handle then writes into the replacement. A file with no
block yet is refused outright (`ident` 0), since nothing tells it from
another.

**Exit**: DATA's entry carries an object id and a generation that nothing
reuses, and `Volume::ident` answers them — the format the track moves DATA to
has inode numbers — with a host test that deletes a held file, makes a
same-length file in its freed block within the second, and has the reopen
refused.
