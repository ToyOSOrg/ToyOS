---
status: open
kind: defect
opened: 2026-09-27
---

# A cached read from a file server costs fifteen to twenty times the kernel's

Measured on QEMU TCG (`Profile::Metal`, DATA on NVMe), 16 MiB on `/home`,
three interleaved runs per arm, the branch that moved DATA to
`/system/bin/fsd` against main at `16d2e645`:

| | main (kernel) | fsd |
|---|---|---|
| write 256 KiB at a time, one fsync | 37–64 MiB/s | 31 MiB/s |
| read back at once, 256 KiB at a time | 1164–1642 MiB/s | 80 MiB/s |
| read on the next boot | 84 MiB/s | 41–42 MiB/s |
| 4 KiB create and fsync, p50 | 1.0–1.3 ms | 3.5–3.6 ms |

The cached read by request size, one run: 47.5 MiB/s at 8 KiB, 91.8 MiB/s at
256 KiB, 182.6 MiB/s at 2 MiB — so about 160 µs a request and about 5 ns a
byte. A byte is copied three times, after a zeroing: out of the block cache
into a buffer the server zeroed (the `READ` arm of `userland/fsd/src/main.rs`,
through `DataVolume::read`), into the client's window a `u64` at a time
with a volatile store each (`toyos::fs::window_put`,
`toyos::volatile::Window::copy_in`), and out of the window a `u64` at a time
again (`window_take`), where the kernel's page cache copies once.

**Exit**: a cached read within a small factor of the kernel's at the same
request size — the block copied once into the window, and the window's word
loops replaced by a copy the compiler may widen — measured by the same
three-run A/B.
