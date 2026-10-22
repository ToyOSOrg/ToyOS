---
status: open
kind: defect
opened: 2026-10-23
---

# The bcachefs reader refuses data extents checksummed crc32c

`lib/bcachefs/src/engine/read.rs`'s `Volume::extent` answers
`Unsupported("data checksum type")` for checksum type 5, the crc32c that an
unencrypted volume's data carries. So the engine cannot read a file stored in
extents on an unencrypted upstream volume: `crc32c.img.gz`'s `seq.txt` is
refused, which `the_crc32c_fixture_reads_its_tree` asserts. Inline data, every
encrypted extent (types 3 and 4) and every node read.

**Why:** the facts the engine was written from, clean-room, name type 5 crc32c,
distinct from the metadata's type 1 "crc32c, non-zero seeded", but give neither
its initial value nor its final XOR. The implementer may not fill a fact from
another source, so the reader refuses rather than guesses.

**Exit:** type 5's computation is established clean-room; the reader verifies it
and the fixture test reads `seq.txt` whole. Writing ToyOS's own unencrypted
extents needs the same fact.

**Owner:** the storage track, before ToyOS writes unencrypted extents.
