---
status: open
kind: defect
opened: 2026-10-04
---

# An image's signed version and its FAT dates are the host's clock

Two builds of one tree write different images, so ToyOS rebuilding itself
cannot reproduce the host's bytes. Two sites in `src/image.rs` read the clock:

- `version_now()`, the signed header's anti-rollback version for a build that
  names none (`src/build.rs`'s plans call it).
- `build_time()`, which dates every file and directory `populate` writes onto
  the ESP and LOG volumes.

ROOT is a function of its files (`root_uuid` hashes them), and
`/system/etc/os-release` records only source facts, its time being the
commit's (`src/build.rs`'s `release`).

**Exit**: both derive from the commit's committer time, as
`TOYOS_COMMIT_TIME` does, or this file records why the signed version cannot.
