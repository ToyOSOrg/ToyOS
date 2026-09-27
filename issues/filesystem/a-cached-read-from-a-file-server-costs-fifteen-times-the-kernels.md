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

## Where a request goes

One guest, the file in fsd's block cache (64 MiB of clean blocks, so all of
it) and the same bytes in the kernel's `/tmp`, three interleaved passes; fsd's
`READ` arm timed from inside the server:

| per request | 4 KiB | 256 KiB | 2 MiB |
|---|---|---|---|
| kernel `/tmp` | 4.2–4.5 µs | 98–100 µs | 751–771 µs |
| fsd, at the client | 115–117 µs | 1906–1922 µs | 4603–4727 µs |
| of which the server's `vec![0; len]` | 3.4 µs | 47–49 µs | 363–490 µs |
| the block cache into it (`DataVolume::read`) | 6.2–6.4 µs | 108–115 µs | 663–665 µs |
| `window_put` into the client's window | 3.7 µs | 790–804 µs | 1794 µs |
| the gap to the next request: reply, the client's `window_take`, the next send | 102–103 µs | 973–989 µs | 1986–1988 µs |

A request that moves no bytes (a one-byte read at the end of the file) costs
104 µs against the kernel's 2.6 µs: that is the round trip, and it is the
whole of a small read. At 256 KiB the two word-at-a-time volatile copies
through the window — `toyos::fs::window_put` in the server and `window_take`
in the client, 2.9–3.5 ns a byte each at 64 KiB to 1 MiB — are about 87% of
the request; the zeroing and the cache copy are 8%, and the round trip 5%.
The same two loops ran at 0.86–0.90 ns a byte on 2 MiB requests and at
0.69–3.17 ns a byte over local memory: under TCG their price per byte depends
on the addresses, a mechanism not isolated here. The kernel's copy is one, at
0.37 ns a byte.

Not the cause: the client holds no cache, but the server's cache answers
every block (the `DataVolume::read` row); and nothing contends for a lock —
the reader is one thread and fsd is one thread over a `RefCell`.

The round trip is what a served read is and does not go; the copies are a
defect. Timings are TCG's: a verdict on their size belongs on metal.

**Exit**: a 256 KiB cached read within a small factor of the kernel's in the
same guest, the block copied once into the window and the window's copies no
longer a scalar volatile loop, measured by the same interleaved runs.
