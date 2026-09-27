---
status: open
kind: tooling
opened: 2026-09-27
---

# A cached read from a file server is measured only under TCG, where a copy out of a shared mapping is slow

One guest (QEMU TCG, the test estate's boot), a 16 MiB file in DATA's block
cache and the same bytes in the kernel's `/tmp`, three interleaved passes of
256 KiB reads:

| per request | fsd | kernel `/tmp` |
|---|---|---|
| at the client | 988–997 µs (251–253 MiB/s) | 99.5–99.8 µs |
| of which the server's `READ` | 107 µs | — |
| a 4 KiB read, the round trip | 153 µs | 4.1 µs |

The server copies each cached block into the client's window once, and the
client copies the window into its buffer once (`toyos::fs::window_take`, one
`copy_nonoverlapping`). In the same guest a 256 KiB `copy_nonoverlapping` out
of a shared-memory mapping into a heap buffer costs 2.86 ns a byte — about
750 µs, the rest of the request — where the same copy between two heap
buffers costs 0.23 ns a byte and a 2 MiB one out of the mapping 0.45 ns. What
under TCG makes a copy out of the mapping slow at 256 KiB is not isolated, and
nothing says whether metal pays it.

**Exit**: the same interleaved runs on the T14, with the copy out of a
shared mapping timed beside a heap-to-heap one; a cost metal also pays is a
defect filed against the mapping, one it does not is closed here.
