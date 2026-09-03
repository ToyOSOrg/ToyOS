---
status: open
kind: defect
opened: 2026-09-03
---

# A 1.9 MB file on `/home` overwritten in place read back as 0 bytes, with the data on the device

Write a 1.9 MB file to `/home`, read it back (correct), overwrite it with 1.9 MB
of the same length, read it back again — **0 bytes**, while the host's readback
off the NVMe image shows the data landed. No loader, no `dlopen`, no mapping: a
`write`/`read`/`write`/`read` on one path.

Reproduced twice during #380's adversarial review, isolated with a no-`dlopen`
control:

    REVIEW-rewrite-nodlopen FAILED: /home/rev-nodl.bin read back 1902104 then 0
    with no dlopen involved

## It did not reproduce here, and the difference is not known

Four samples on `w5b14-ruled` at the shape that became #380, in the
`so_cache_refusals` boot (`Profile::MetalDisk`, `/home` on NVMe, not tmpfs —
the boot log's `/home is a tmpfs` line is absent), through a probe with the same
shape:

    /home/rev-nodl.bin wrote 1902112 twice, read back 1902112 then 1902112
    /home/rev-nodl.bin wrote 1902112 twice, read back 1902112 then 1902112,
    metadata says 1902112

Twice with the probe running last, after three `dlopen` arms had already written
to `/home`, and twice with it running first before any `dlopen` at all; each of
those pairs is one parallel run and one `ALONE` re-run. So it is not simply
"any same-length overwrite on `/home`", and what separates the reviewer's shape
from this one is unestablished. The byte counts differ by 8 (1,902,104 against
1,902,112), which is two builds of `libtls_lib.so` and not thought to matter.

## Reproduction recipe

1. Boot a config whose `/home` is NVMe-backed (`Profile::MetalDisk`); confirm
   `/home is a tmpfs` is **not** in the boot log, or the arm judges nothing.
2. In a guest binary: `fs::write(p, &bytes)` with `bytes.len()` about 1.9 MB,
   `fs::read(p).len()`, `fs::write(p, &bytes)` again, `fs::read(p).len()`.
3. Report both lengths. The second is the defect when it is 0.
4. Then `run shutdown` and read `p` off the NVMe image with the host's own
   `bcachefs` reader (`tests/common/storage.rs`'s `FileBlocks`), which is what
   says the bytes reached the device while the guest read none.

## Where to look

Not established, and the two nearest records do not state this:
`issues/filesystem/a-page-faulted-through-an-old-backing-is-nobodys.md` is about
a backing taken before a write and the fault path that reads through it — no
mapping is involved here — and
`issues/kernel/ftruncate-answers-wouldblock-and-nothing-retries-it.md` is a
refused shrink that changes nothing, where this loses a whole file's length.
A same-length overwrite is the one shape that changes no metadata a reader keys
on, so the file cache's own length for the path is the first thing to read.
