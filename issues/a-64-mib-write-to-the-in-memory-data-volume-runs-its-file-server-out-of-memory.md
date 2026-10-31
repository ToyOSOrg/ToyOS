---
status: open
kind: defect
opened: 2026-10-21
---

# A 64 MiB write to the in-memory DATA volume runs its file server out of memory

On a boot with no DATA partition the DATA file server formats a volume in
memory (`system/fileserver/src/main.rs`, `RAM_BLOCKS`: 1 GiB, sparse). Writing
one 64 MiB file to it through std (`osbench files`, `fs.served.seq_write.4096`:
`File::create`, then 16,384 writes of 4 KiB) ends the server:

```
fileserver error] memory allocation of 4096 bytes failed
cpu0 kernel] exit: fileserver pid=4 code=134 cpu=11326ms peak=3724MB allocs=1863 ... 63=1864 ...
supervisor] supervisor: fileserver data (pid 4) ended; started again as pid 11
```

QEMU TCG, 4 vCPUs, 4 GiB of memory, the benchcase image (`tests/images/benchcase.toml`)
built from `wt/toyos-bench2`. Before it, the same process had served a 16 MiB
file, 2,000 4 KiB writes into it and a few thousand small creates and stats.
The restarted server died the same way within its first 88 requests of the
1 MiB-call variant of the same write (`peak=3724MB`). The 1 GiB volume and a
cache bounded for clean blocks account for at most a little over 1 GiB; the
server's heap reached every byte the machine had.

The growth is not isolated. The server's own allocations per write are a
block (`cache.write` of a dirty block, `data.rs`'s `write`) and an allocation
in the volume's metadata per page grown (`resolve_or_alloc_block` then;
`Mounted::allocate` now allocates a write's pages at once); a
metadata update that writes a whole btree node through the unbounded dirty
side of the cache per page would come to gigabytes for 16,384 pages, but that
is an inference from the code, not a measurement.

On metal the server survived: all six osbench boots of round 2's
`e5573b7fe` (three each on the T14 and the AMD bench, both with more memory
than the 4 GiB guest) wrote the file and exited 0. One did not do it cleanly:
on the T14's first boot a `fs.served.seq_write.1048576` round of the same
write ran under 1 MB/s (`min=0`), and `files` took 80 s against the other
boots' 13 s, so the same write stalls on metal even where it completes.

The server also refuses nothing on the way: every request answered until the
allocator aborted the process, and the client saw `the file server is gone`.

The same write is now `osbench persist`'s `fs.disk.seq_write.*`, which
reaches the in-memory volume on a boot whose DATA is not designated.

**Exit**: the server's memory for a 64 MiB file written to the in-memory volume
is bounded by the file's size plus a stated overhead, shown by the same write
under QEMU with its peak in the kernel's exit record; or the server refuses
the write that would exceed what it may hold, by name.

Owner: the file server (`system/fileserver/`).
